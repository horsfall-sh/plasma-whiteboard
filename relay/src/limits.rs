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

    pub fn full(&mut self) -> bool {
        self.refill();
        self.tokens >= self.cap
    }
}

/// One limit per IPv4 address or per IPv6 /64 (a home or phone gets a whole /64).
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct IpKey([u8; 16]);

fn key(ip: IpAddr) -> IpKey {
    match ip {
        IpAddr::V4(v4) => IpKey(v4.to_ipv6_mapped().octets()),
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => IpKey(v4.to_ipv6_mapped().octets()),
            None => {
                let mut o = v6.octets();
                o[8..].fill(0);
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
}

#[derive(Default, Clone)]
pub struct IpLimits {
    inner: Arc<Mutex<Inner>>,
}

/// Holds one connection slot for an address; frees it when dropped.
pub struct IpGuard {
    inner: Arc<Mutex<Inner>>,
    key: IpKey,
    out: Arc<Mutex<Bucket>>,
}

impl IpGuard {
    /// The address's budget for replies it asked for.
    pub fn out(&self) -> Arc<Mutex<Bucket>> {
        self.out.clone()
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
}

impl Budget {
    pub fn new(out: Arc<Mutex<Bucket>>) -> Budget {
        Budget { heavy: Bucket::new(12.0, 0.5), out }
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
        Ok(IpGuard { inner: self.inner.clone(), key: k, out })
    }

    /// Forget addresses whose bucket has refilled.
    pub fn prune(&self) {
        let mut g = self.inner.lock().unwrap();
        g.creates.retain(|_, b| !b.full());
        g.joins.retain(|_, b| !b.full());
        g.layers.retain(|_, b| !b.full());
        let Inner { conns, outs, .. } = &mut *g;
        outs.retain(|k, b| conns.contains_key(k) || !b.lock().unwrap().full());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn ipv6_is_limited_by_slash_64() {
        let l = IpLimits::default();
        let a: IpAddr = "2001:db8:1:2::1".parse().unwrap();
        let b: IpAddr = "2001:db8:1:2:ffff::9".parse().unwrap();
        for _ in 0..10 {
            assert!(l.may_create(a));
        }
        assert!(!l.may_create(b));
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
        let (mut a, mut b) = (Budget::new(g1.out()), Budget::new(g2.out()));
        let half = (OUT_BURST / 2.0) as usize;
        assert!(a.send(half) && b.send(half));
        assert!(!a.send(64 * 1024) && !b.send(64 * 1024), "both connections drew on one budget");
        drop((a, b, g1, g2));
        l.prune();
        let mut c = Budget::new(l.connect(ip).unwrap().out());
        assert!(!c.send(64 * 1024), "a new connection does not get a fresh budget");
        let other = IpAddr::from([198, 51, 100, 3]);
        assert!(Budget::new(l.connect(other).unwrap().out()).send(half));
    }
}
