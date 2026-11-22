//! The boards loaded in memory. Each board has its own load cell, so the
//! registry lock is held only to find the cell, never while a board is read
//! from storage or joined: one slow board does not stall the others.

use crate::db::Db;
use crate::memory::{weight, Memory};
use crate::room::{Joined, Room};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// How long a board stays loaded after its last connection leaves, so a
/// reconnect (or a leave and join loop) does not read it again each time.
pub const UNLOAD_GRACE: Duration = Duration::from_secs(15);

/// Boards read from storage at once: each read holds a whole board in
/// memory before the budget can weigh it.
const MAX_LOADING: usize = 2;

#[derive(Default)]
struct Cell {
    room: tokio::sync::Mutex<Option<Arc<Room>>>,
}

/// Why a join was refused.
#[derive(Debug, PartialEq, Eq)]
pub enum Refused {
    /// The board could not be read from storage.
    Unreadable,
    /// The room said no (it is full).
    Full,
    /// Loading it would go over the shared memory budget.
    Busy,
}

pub struct Rooms {
    map: Mutex<HashMap<i64, Arc<Cell>>>,
    grace: Duration,
    mem: Arc<Memory>,
    loading: tokio::sync::Semaphore,
}

impl Rooms {
    pub fn new(grace: Duration, mem: Memory) -> Arc<Rooms> {
        Arc::new(Rooms {
            map: Mutex::new(HashMap::new()),
            grace,
            mem: Arc::new(mem),
            loading: tokio::sync::Semaphore::new(MAX_LOADING),
        })
    }

    /// Unload boards no one is connected to (ones in their grace period),
    /// other than `keep`, until `fits` says yes. Cells being joined or
    /// loaded are skipped, never waited on.
    fn make_room(&self, keep: i64, fits: impl Fn() -> bool) -> bool {
        let cells: Vec<(i64, Arc<Cell>)> =
            self.map.lock().unwrap().iter().filter(|(b, _)| **b != keep).map(|(b, c)| (*b, c.clone())).collect();
        for (board, cell) in cells {
            if fits() {
                return true;
            }
            let Ok(mut slot) = cell.room.try_lock() else { continue };
            if slot.as_ref().is_some_and(|r| r.conns() == 0) {
                // Dropping the last hold on it gives its memory back.
                *slot = None;
                self.remove(board, &cell);
            }
        }
        fits()
    }

    /// The boards loaded or being loaded.
    pub fn boards(&self) -> Vec<i64> {
        self.map.lock().unwrap().keys().copied().collect()
    }

    fn is_current(&self, board: i64, cell: &Arc<Cell>) -> bool {
        self.map.lock().unwrap().get(&board).is_some_and(|c| Arc::ptr_eq(c, cell))
    }

    /// Drop the cell from the registry. Call with its lock held and its slot empty.
    fn remove(&self, board: i64, cell: &Arc<Cell>) {
        let mut map = self.map.lock().unwrap();
        if map.get(&board).is_some_and(|c| Arc::ptr_eq(c, cell)) {
            map.remove(&board);
        }
    }

    /// Find or load the board, then join it with `join`. A lost room (one
    /// whose write failed) is read again rather than joined.
    pub async fn join(
        &self,
        db: &Db,
        board: i64,
        join: impl FnOnce(&Room) -> Result<Joined, ()>,
    ) -> Result<(Arc<Room>, Joined), Refused> {
        loop {
            let cell = self.map.lock().unwrap().entry(board).or_default().clone();
            let mut slot = cell.room.lock().await;
            if !self.is_current(board, &cell) {
                // Unloaded while this one waited: start again with the new cell.
                continue;
            }
            let room = match slot.as_ref().filter(|r| !r.is_dead()) {
                Some(r) => r.clone(),
                None => {
                    // A board that can't be read is refused, never loaded empty.
                    // Weigh it before reading it: a board that can't fit is
                    // refused without the cost of loading it.
                    let est = db.size(board).await.map(weight);
                    let fits = est.is_some_and(|w| self.mem.fits(w) || self.make_room(board, || self.mem.fits(w)));
                    if !fits {
                        if slot.as_ref().is_none_or(|r| r.conns() == 0) {
                            *slot = None;
                            self.remove(board, &cell);
                        }
                        return Err(if est.is_none() { Refused::Unreadable } else { Refused::Busy });
                    }
                    let Ok(_turn) = self.loading.acquire().await else { return Err(Refused::Busy) };
                    let Ok(loaded) = db.load(board).await else {
                        if slot.as_ref().is_none_or(|r| r.conns() == 0) {
                            *slot = None;
                            self.remove(board, &cell);
                        }
                        return Err(Refused::Unreadable);
                    };
                    let r = Arc::new(Room::new(board, loaded, self.mem.clone()));
                    if !r.admit() && !self.make_room(board, || r.admit()) {
                        if slot.as_ref().is_none_or(|r| r.conns() == 0) {
                            *slot = None;
                            self.remove(board, &cell);
                        }
                        return Err(Refused::Busy);
                    }
                    *slot = Some(r.clone());
                    r
                }
            };
            let Ok(joined) = join(&room) else {
                if room.conns() == 0 {
                    *slot = None;
                    self.remove(board, &cell);
                }
                return Err(Refused::Full);
            };
            return Ok((room, joined));
        }
    }

    /// A connection has left `room`. Unload it once it has had no
    /// connection for the grace period.
    pub fn left(self: &Arc<Self>, room: Arc<Room>) {
        if room.conns() > 0 {
            return;
        }
        let rooms = self.clone();
        let board = room.board;
        // Not a strong hold: an idle board unloaded early (to make room for
        // another) must free its memory now, not when this timer ends.
        let room = Arc::downgrade(&room);
        tokio::spawn(async move {
            tokio::time::sleep(rooms.grace).await;
            let Some(cell) = rooms.map.lock().unwrap().get(&board).cloned() else {
                return;
            };
            let mut slot = cell.room.lock().await;
            // Joins happen under this lock, so the count can't change under us.
            if slot.as_ref().is_some_and(|r| std::ptr::eq(Arc::as_ptr(r), room.as_ptr()) && r.conns() == 0) {
                *slot = None;
                rooms.remove(board, &cell);
            }
        });
    }

    /// Loaded rooms, for tests.
    pub fn loaded(&self) -> usize {
        self.map.lock().unwrap().len()
    }

    /// The shared budget's bytes and boards held, for tests.
    pub fn memory(&self) -> (usize, usize) {
        self.mem.used()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Role;

    fn temp_db() -> Db {
        let mut r = [0u8; 6];
        getrandom::fill(&mut r).unwrap();
        let hex: String = r.iter().map(|b| format!("{b:02x}")).collect();
        let p = std::env::temp_dir().join(format!("wb-unit-rooms-{hex}.db"));
        Db::open(&p.to_string_lossy()).unwrap()
    }

    #[tokio::test]
    async fn a_board_being_loaded_does_not_hold_up_another() {
        let db = temp_db();
        let a = db.create_board([1; 32], [2; 32]).await.unwrap();
        let b = db.create_board([3; 32], [4; 32]).await.unwrap();
        let rooms = Rooms::new(Duration::from_secs(60), Memory::new(usize::MAX / 2, 100));
        // Stand in for a slow load of board a: hold its cell.
        let cell = rooms.map.lock().unwrap().entry(a).or_default().clone();
        let held = cell.room.lock().await;
        let joined = tokio::time::timeout(
            Duration::from_secs(2),
            rooms.join(&db, b, |r| r.join(1, Role::View, None, None, &db)),
        )
        .await;
        assert!(matches!(joined, Ok(Ok(_))), "board b joined while board a was loading");
        drop(held);
        assert!(rooms.join(&db, a, |r| r.join(2, Role::View, None, None, &db)).await.is_ok());
        assert_eq!(rooms.loaded(), 2);
    }

    #[tokio::test]
    async fn an_edit_key_holder_turned_viewer_still_gets_the_view_key() {
        use crate::room::MAX_LAYERS;
        let db = temp_db();
        let a = db.create_board([7; 32], [8; 32]).await.unwrap();
        let rooms = Rooms::new(Duration::from_secs(60), Memory::new(usize::MAX / 2, 100));
        let (room, _) = rooms.join(&db, a, |r| r.join(0, Role::View, None, None, &db)).await.unwrap();
        for n in 0..MAX_LAYERS {
            let mut who = [0u8; 32];
            who[..8].copy_from_slice(&(n as u64).to_le_bytes());
            let j = room.join(n as u64 + 1, Role::Edit, Some(who), Some("vk"), &db).unwrap();
            room.leave(j.peer);
        }
        let j = room.join(9999, Role::Edit, Some([0xee; 32]), Some("vk"), &db).unwrap();
        assert_eq!(j.peer.role, Role::View, "the layers are full");
        let hello: serde_json::Value = serde_json::from_str(j.snapshot[0].as_str()).unwrap();
        assert_eq!(hello["role"], "view");
        assert_eq!(hello["view"], "vk", "the view link must not fall back to the edit key");
    }

    #[tokio::test]
    async fn a_join_that_waited_on_an_unloaded_cell_starts_again() {
        let db = temp_db();
        let a = db.create_board([5; 32], [6; 32]).await.unwrap();
        let rooms = Rooms::new(Duration::ZERO, Memory::new(usize::MAX / 2, 100));
        let (room, j) = rooms.join(&db, a, |r| r.join(1, Role::View, None, None, &db)).await.unwrap();
        let old = rooms.map.lock().unwrap().get(&a).cloned().unwrap();
        let mut held = old.room.lock().await;
        // A joiner queues on the cell while the last connection leaves and
        // the board is unloaded (what `left` does after the grace).
        let r2 = rooms.clone();
        let db2 = db.clone();
        let waiter = tokio::spawn(async move {
            r2.join(&db2, a, |r| r.join(2, Role::View, None, None, &db2)).await.map(|(r, _)| r)
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        room.leave(j.peer);
        *held = None;
        rooms.remove(a, &old);
        drop(held);
        let joined = waiter.await.unwrap().unwrap();
        let now = rooms.map.lock().unwrap().get(&a).cloned().unwrap();
        let in_registry = now.room.lock().await.clone().unwrap();
        assert!(Arc::ptr_eq(&joined, &in_registry), "the waiter joined the room the registry holds");
        assert!(!Arc::ptr_eq(&joined, &room), "not the unloaded one");
        assert!(!Arc::ptr_eq(&now, &old));
        assert_eq!(joined.conns(), 1);
    }
}
