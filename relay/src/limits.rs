//! Rate limits: a token bucket, and per-address counters for new boards and
//! open connections.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use std::time::Instant;

pub const MAX_CONNS_PER_IP: u32 = 40;
/// What one address may be sent in replies it asked for (sheet previews): a burst of two
/// full boards, then 1 MB a second, across all its connections.
pub const OUT_BURST: f64 = 32.0 * 1024.0 * 1024.0;
pub const OUT_PER_SEC: f64 = 1024.0 * 1024.0;
/// What one board may broadcast of whole sheets (bring back, layer restore),
/// whoever asks: three full boards at once, then 1 MB a second. Each is sent
/// to everyone on the board, so this, not a per-connection count, is what
/// bounds the traffic one board can be made to send.
pub const SHEETS_BURST: f64 = 48.0 * 1024.0 * 1024.0;
pub const SHEETS_PER_SEC: f64 = 1024.0 * 1024.0;
/// Connection attempts (each one a join) per address: a class of 40 opening
/// the page at once, then two a second.
const JOIN_BURST: f64 = 60.0;
const JOIN_PER_SEC: f64 = 2.0;
/// New contributor layers one address may be given, across all boards: a
/// class opening a fresh board at once, then one every half minute.
const LAYER_BURST: f64 = 60.0;
const LAYER_PER_SEC: f64 = 1.0 / 30.0;
const CREATE_BURST: f64 = 10.0;
const CREATE_PER_SEC: f64 = 0.1;

pub struct Bucket {
    tokens: f64,
    cap: f64,
    per_sec: f64,
    at: Instant,
}

impl Bucket {
    pub fn new(cap: f64, per_sec: f64) -> Bucket {
        Bucket { tokens: cap, cap, per_sec, at: Instant::now() }
    }

    fn refill(&mut self) {
        let now = Instant::now();
        self.tokens = (self.tokens + now.duration_since(self.at).as_secs_f64() * self.per_sec).min(self.cap);
        self.at = now;
    }

    pub fn take(&mut self) -> bool {
        self.take_n(1.0)
    }

    /// Take `n` tokens at once (bytes, for a byte-rate limit).
    pub fn take_n(&mut self, n: f64) -> bool {
        self.refill();
        if self.tokens >= n {
            self.tokens -= n;
            true
        } else {
            false
        }
    }

    /// Put back `n` tokens taken for something that did not happen after all.
    pub fn give(&mut self, n: f64) {
        self.tokens = (self.tokens + n).min(self.cap);
    }

    pub fn full(&mut self) -> bool {
        self.refill();
        self.tokens >= self.cap
    }
}

/// One limit per IPv4 address or per IPv6 /56. A home is usually given a /56
/// and a phone a /64; keying by /64 would hand a /48 holder 65,536 times
/// every limit, where a /56 key leaves it 256.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct IpKey([u8; 16]);

fn key(ip: IpAddr) -> IpKey {
    match ip {
        IpAddr::V4(v4) => IpKey(v4.to_ipv6_mapped().octets()),
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => IpKey(v4.to_ipv6_mapped().octets()),
            None => {
                let mut o = v6.octets();
                o[7..].fill(0);
                IpKey(o)
            }
        },
    }
}

#[derive(Default)]
struct Inner {
    creates: HashMap<IpKey, Bucket>,
    joins: HashMap<IpKey, Bucket>,
    layers: HashMap<IpKey, Bucket>,
    conns: HashMap<IpKey, u32>,
    /// Kept after the last connection closes, so reconnecting does not refill it.
    outs: HashMap<IpKey, Arc<Mutex<Bucket>>>,
    /// Bytes of strokes the address may still store; kept like `outs`.
    stores: HashMap<IpKey, Arc<Mutex<Bucket>>>,
}

/// What one address may store, as a share of the storage quota: it can hold at most this
/// much of it at a time, so no one address can fill storage for everyone (council H5).
pub const STORE_SHARE: u64 = 16;

#[derive(Clone)]
pub struct IpLimits {
    inner: Arc<Mutex<Inner>>,
    /// Each address's storage bucket: capacity in bytes and refill per second.
    store: (f64, f64),
}

impl Default for IpLimits {
    /// The default quota, refilled over the default 90 days.
    fn default() -> IpLimits {
        IpLimits::new(crate::db::MAX_DB, 90)
    }
}

/// Holds one connection slot for an address; frees it when dropped.
pub struct IpGuard {
    inner: Arc<Mutex<Inner>>,
    key: IpKey,
    out: Arc<Mutex<Bucket>>,
    store: Arc<Mutex<Bucket>>,
}

impl IpGuard {
    /// The address's budget for replies it asked for.
    pub fn out(&self) -> Arc<Mutex<Bucket>> {
        self.out.clone()
    }

    /// A new connection's budgets, sharing the address's reply and storage buckets.
    pub fn budget(&self) -> Budget {
        Budget::new(self.out.clone(), self.store.clone())
    }

    /// Whether the address may be given another new contributor layer.
    pub fn may_add_layer(&self) -> bool {
        let mut g = self.inner.lock().unwrap();
        g.layers.entry(self.key).or_insert_with(|| Bucket::new(LAYER_BURST, LAYER_PER_SEC)).take()
    }
}

/// A connection's budgets for the messages that cost the relay a whole sheet.
pub struct Budget {
    /// Whole-sheet operations (crumple, bring back, preview, layer restore), per connection.
    pub heavy: Bucket,
    /// Bytes of requested replies, shared by the address's connections.
    pub out: Arc<Mutex<Bucket>>,
    /// Bytes of strokes the address may still store, shared by its connections (H5).
    pub store: Arc<Mutex<Bucket>>,
}

impl Budget {
    pub fn new(out: Arc<Mutex<Bucket>>, store: Arc<Mutex<Bucket>>) -> Budget {
        Budget { heavy: Bucket::new(12.0, 0.5), out, store }
    }

    /// Take `n` bytes from the reply budget.
    pub fn send(&mut self, n: usize) -> bool {
        self.out.lock().unwrap().take_n(n as f64)
    }
}

impl Drop for IpGuard {
    fn drop(&mut self) {
        let mut g = self.inner.lock().unwrap();
        if let Some(n) = g.conns.get_mut(&self.key) {
            *n -= 1;
            if *n == 0 {
                g.conns.remove(&self.key);
            }
        }
    }
}

impl IpLimits {
    /// Limits for storage of `max_db` bytes whose boards are kept `ttl_days` after they are
    /// last opened. Each address may hold a [`STORE_SHARE`]th of it, refilled over the TTL
    /// (90 days when boards are kept for ever).
    pub fn new(max_db: u64, ttl_days: u32) -> IpLimits {
        let cap = (max_db / STORE_SHARE) as f64;
        let days = if ttl_days == 0 { 90 } else { ttl_days };
        IpLimits { inner: Arc::default(), store: (cap, cap / (f64::from(days) * 86_400.0)) }
    }

    pub fn may_create(&self, ip: IpAddr) -> bool {
        let mut g = self.inner.lock().unwrap();
        g.creates
            .entry(key(ip))
            .or_insert_with(|| Bucket::new(CREATE_BURST, CREATE_PER_SEC))
            .take()
    }

    /// Open a connection slot. Err: why not, for the client.
    pub fn connect(&self, ip: IpAddr) -> Result<IpGuard, &'static str> {
        let k = key(ip);
        let mut g = self.inner.lock().unwrap();
        if g.conns.get(&k).is_some_and(|n| *n >= MAX_CONNS_PER_IP) {
            return Err("too many connections from this address");
        }
        if !g.joins.entry(k).or_insert_with(|| Bucket::new(JOIN_BURST, JOIN_PER_SEC)).take() {
            return Err("connecting too often; wait a moment");
        }
        *g.conns.entry(k).or_insert(0) += 1;
        let out = g.outs.entry(k).or_insert_with(|| Arc::new(Mutex::new(Bucket::new(OUT_BURST, OUT_PER_SEC)))).clone();
        let (cap, per_sec) = self.store;
        let store = g.stores.entry(k).or_insert_with(|| Arc::new(Mutex::new(Bucket::new(cap, per_sec)))).clone();
        Ok(IpGuard { inner: self.inner.clone(), key: k, out, store })
    }

    /// Forget addresses whose bucket has refilled.
    pub fn prune(&self) {
        let mut g = self.inner.lock().unwrap();
        g.creates.retain(|_, b| !b.full());
        g.joins.retain(|_, b| !b.full());
        g.layers.retain(|_, b| !b.full());
        let Inner { conns, outs, stores, .. } = &mut *g;
        outs.retain(|k, b| conns.contains_key(k) || !b.lock().unwrap().full());
        stores.retain(|k, b| conns.contains_key(k) || !b.lock().unwrap().full());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Council H5: one address holds at most its share of storage, across its connections,
    /// a reconnect and a prune; another address has its own.
    #[test]
    fn an_address_stores_at_most_its_share() {
        let l = IpLimits::new(16 * 1000, 90);
        let a: IpAddr = "2001:db8:1::1".parse().unwrap();
        let a2: IpAddr = "2001:db8:1:ff::2".parse().unwrap(); // the same /56
        let b: IpAddr = "2001:db8:2::1".parse().unwrap();
        let (g1, g2) = (l.connect(a).unwrap(), l.connect(a2).unwrap());
        assert!(g1.budget().store.lock().unwrap().take_n(600.0));
        assert!(!g2.budget().store.lock().unwrap().take_n(600.0), "shared by the address's connections");
        drop((g1, g2));
        l.prune();
        assert!(!l.connect(a).unwrap().budget().store.lock().unwrap().take_n(600.0), "not refilled by reconnecting");
        assert!(l.connect(b).unwrap().budget().store.lock().unwrap().take_n(1000.0), "another address has its own");
    }

    #[test]
    fn create_limit_is_per_address() {
        let l = IpLimits::default();
        let a: IpAddr = "203.0.113.7".parse().unwrap();
        let b: IpAddr = "203.0.113.8".parse().unwrap();
        for _ in 0..10 {
            assert!(l.may_create(a));
        }
        assert!(!l.may_create(a));
        assert!(l.may_create(b));
    }

    #[test]
    fn ipv6_is_limited_by_slash_56() {
        let l = IpLimits::default();
        let a: IpAddr = "2001:db8:1:200::1".parse().unwrap();
        // Another /64 in the same /56.
        let b: IpAddr = "2001:db8:1:2ff:ffff::9".parse().unwrap();
        // The next /56.
        let c: IpAddr = "2001:db8:1:300::1".parse().unwrap();
        for _ in 0..10 {
            assert!(l.may_create(a));
        }
        assert!(!l.may_create(b));
        assert!(l.may_create(c));
    }

    #[test]
    fn connection_slots_are_returned() {
        let l = IpLimits::default();
        let ip: IpAddr = "198.51.100.1".parse().unwrap();
        let guards: Vec<_> = (0..MAX_CONNS_PER_IP).map(|_| l.connect(ip).unwrap()).collect();
        assert!(l.connect(ip).is_err());
        drop(guards);
        assert!(l.connect(ip).is_ok());
    }

    #[test]
    fn connection_attempts_are_rate_limited_per_address() {
        let l = IpLimits::default();
        let ip = IpAddr::from([198, 51, 100, 4]);
        for _ in 0..JOIN_BURST as usize {
            drop(l.connect(ip).unwrap());
        }
        assert_eq!(l.connect(ip).err(), Some("connecting too often; wait a moment"));
        assert!(l.connect(IpAddr::from([198, 51, 100, 5])).is_ok());
    }

    #[test]
    fn the_reply_budget_is_shared_by_an_address_and_outlives_its_connections() {
        let l = IpLimits::default();
        let ip = IpAddr::from([198, 51, 100, 2]);
        let (g1, g2) = (l.connect(ip).unwrap(), l.connect(ip).unwrap());
        let (mut a, mut b) = (g1.budget(), g2.budget());
        let half = (OUT_BURST / 2.0) as usize;
        assert!(a.send(half) && b.send(half));
        assert!(!a.send(64 * 1024) && !b.send(64 * 1024), "both connections drew on one budget");
        drop((a, b, g1, g2));
        l.prune();
        let mut c = l.connect(ip).unwrap().budget();
        assert!(!c.send(64 * 1024), "a new connection does not get a fresh budget");
        let other = IpAddr::from([198, 51, 100, 3]);
        assert!(l.connect(other).unwrap().budget().send(half));
    }
}
