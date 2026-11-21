//! Rate limits: a token bucket, and per-address counters for new boards and
//! open connections.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use std::time::Instant;

pub const MAX_CONNS_PER_IP: u32 = 40;
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
        self.refill();
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }

    fn full(&mut self) -> bool {
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
    conns: HashMap<IpKey, u32>,
}

#[derive(Default, Clone)]
pub struct IpLimits {
    inner: Arc<Mutex<Inner>>,
}

/// Holds one connection slot for an address; frees it when dropped.
pub struct IpGuard {
    inner: Arc<Mutex<Inner>>,
    key: IpKey,
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

    pub fn connect(&self, ip: IpAddr) -> Option<IpGuard> {
        let k = key(ip);
        let mut g = self.inner.lock().unwrap();
        let n = g.conns.entry(k).or_insert(0);
        if *n >= MAX_CONNS_PER_IP {
            return None;
        }
        *n += 1;
        Some(IpGuard { inner: self.inner.clone(), key: k })
    }

    /// Forget addresses whose bucket has refilled.
    pub fn prune(&self) {
        self.inner.lock().unwrap().creates.retain(|_, b| !b.full());
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
        assert!(l.connect(ip).is_none());
        drop(guards);
        assert!(l.connect(ip).is_some());
    }
}
