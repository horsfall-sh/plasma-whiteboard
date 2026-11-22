//! What all loaded boards may hold in memory together. The per-board caps
//! alone would let enough boards, each under its own cap, exhaust the
//! process; this budget is shared by every board and fails closed.

use std::sync::Mutex;

/// The default for everything loaded at once, in bytes of weight.
pub const MAX_LOADED: usize = 144 * 1024 * 1024;
/// The default for boards loaded at once.
pub const MAX_ROOMS: usize = 2000;
/// A loaded board's fixed cost: its maps, layers and broadcast queue.
pub const ROOM_COST: usize = 64 * 1024;
/// Each byte of stroke JSON is held about this many times: the stroke
/// itself, the snapshot cache built from it, a sheet preview and allocator
/// slack. Measured with the ignored `memory_harness` test.
pub const COPIES: usize = 4;

/// The weight of a board holding `bytes` of stroke JSON.
pub fn weight(bytes: usize) -> usize {
    ROOM_COST + COPIES * bytes
}

#[derive(Default)]
struct Used {
    bytes: usize,
    rooms: usize,
}

pub struct Memory {
    used: Mutex<Used>,
    max_bytes: usize,
    max_rooms: usize,
}

impl Memory {
    pub fn new(max_bytes: usize, max_rooms: usize) -> Memory {
        Memory { used: Mutex::new(Used::default()), max_bytes, max_rooms }
    }

    /// Take a board of `weight` in, or say no if it does not fit.
    pub fn admit(&self, weight: usize) -> bool {
        let mut u = self.used.lock().unwrap();
        if u.rooms >= self.max_rooms || u.bytes + weight > self.max_bytes {
            return false;
        }
        u.rooms += 1;
        u.bytes += weight;
        true
    }

    /// Whether a board of `weight` would be admitted now.
    pub fn fits(&self, weight: usize) -> bool {
        let u = self.used.lock().unwrap();
        u.rooms < self.max_rooms && u.bytes + weight <= self.max_bytes
    }

    /// A loaded board grows by `n`, if it fits.
    pub fn grow(&self, n: usize) -> bool {
        let mut u = self.used.lock().unwrap();
        if u.bytes + n > self.max_bytes {
            return false;
        }
        u.bytes += n;
        true
    }

    /// A loaded board shrank by `n`.
    pub fn shrink(&self, n: usize) {
        let mut u = self.used.lock().unwrap();
        u.bytes = u.bytes.saturating_sub(n);
    }

    /// A board of `weight` was let go.
    pub fn release(&self, weight: usize) {
        let mut u = self.used.lock().unwrap();
        u.rooms = u.rooms.saturating_sub(1);
        u.bytes = u.bytes.saturating_sub(weight);
    }

    /// Bytes and boards held, for tests.
    pub fn used(&self) -> (usize, usize) {
        let u = self.used.lock().unwrap();
        (u.bytes, u.rooms)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_budget_holds_and_gives_back() {
        let m = Memory::new(weight(100), 2);
        assert!(m.admit(weight(60)));
        assert!(!m.admit(weight(60)), "two would not fit");
        assert!(!m.grow(COPIES * 41));
        assert!(m.grow(COPIES * 40));
        m.shrink(COPIES * 40);
        m.release(weight(60));
        assert_eq!(m.used(), (0, 0));
        let m = Memory::new(usize::MAX / 2, 1);
        assert!(m.admit(1));
        assert!(!m.admit(1), "the board count is capped too");
    }
}
