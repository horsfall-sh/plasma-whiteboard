//! SQLite storage. One thread owns the connection and takes commands in order,
//! so the relay's async tasks never block on disk and every read sees every
//! write queued before it. Commands that arrive together share a transaction.
//!
//! A write a room makes carries the room's [`Link`]. Its acknowledgement runs
//! only once the transaction holding it has committed. If the write or the
//! commit fails, the room is told it is lost: what it holds in memory is no
//! longer what is stored, so it must be dropped and read again.

use rusqlite::{params, Connection};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::oneshot;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Role {
    Edit,
    View,
}

pub const LIVE: u8 = 0; // drawn
pub const UNDONE: u8 = 1; // removed by its author; only the id is remembered
pub const DELETED: u8 = 2; // removed with its layer; can be restored

pub struct StrokeRow {
    pub seq: u64,
    pub id: String,
    pub layer: u32,
    pub color: String,
    pub width: f64,
    pub pts: String,
    /// Set for a text annotation; None for a pen stroke.
    pub txt: Option<String>,
    pub state: u8,
    /// 0: the sheet being drawn on. Above 0: a crumpled sheet on the shelf.
    pub sheet: u32,
}

#[derive(Default)]
pub struct Loaded {
    pub layers: Vec<(u32, [u8; 32], String)>,
    pub strokes: Vec<StrokeRow>,
}

/// A room's end of the storage queue.
pub struct Link {
    pub board: i64,
    dead: AtomicBool,
    on_lost: Box<dyn Fn() + Send + Sync>,
}

impl Link {
    /// `on_lost` runs once, on the storage thread, when a write is lost.
    pub fn new(board: i64, on_lost: impl Fn() + Send + Sync + 'static) -> Arc<Link> {
        Arc::new(Link { board, dead: AtomicBool::new(false), on_lost: Box::new(on_lost) })
    }

    /// A write of this room's failed: it must not be used any more.
    pub fn is_dead(&self) -> bool {
        self.dead.load(Ordering::SeqCst)
    }

    fn lose(&self) {
        if !self.dead.swap(true, Ordering::SeqCst) {
            (self.on_lost)();
        }
    }
}

/// Runs on the storage thread once the write it came with is committed.
pub type Ack = Box<dyn FnOnce() + Send>;

struct Job {
    cmd: Cmd,
    /// Set for a room's write.
    link: Option<Arc<Link>>,
    ack: Option<Ack>,
}

enum Cmd {
    /// Nothing to write: its ack waits for everything queued before it.
    Nop,
    CreateBoard {
        edit: [u8; 32],
        view: [u8; 32],
        reply: oneshot::Sender<Option<i64>>,
    },
    Lookup {
        hash: [u8; 32],
        reply: oneshot::Sender<Option<(i64, Role)>>,
    },
    Load {
        board: i64,
        reply: oneshot::Sender<Result<Loaded, ()>>,
    },
    AddLayer {
        board: i64,
        n: u32,
        who: [u8; 32],
        color: String,
    },
    AddStroke {
        board: i64,
        seq: u64,
        id: String,
        layer: u32,
        color: String,
        width: f64,
        pts: String,
        txt: Option<String>,
    },
    StrokeState {
        board: i64,
        id: String,
        state: u8,
    },
    LayerState {
        board: i64,
        layer: u32,
        from: u8,
        to: u8,
    },
    SheetMove {
        board: i64,
        from: u32,
        to: u32,
    },
    SheetDrop {
        board: i64,
        sheet: u32,
    },
    Sweep {
        cutoff: i64,
        skip: Vec<i64>,
        reply: oneshot::Sender<usize>,
    },
    Flush {
        reply: oneshot::Sender<()>,
    },
}

/// The default storage quota, in bytes.
pub const MAX_DB: u64 = 1024 * 1024 * 1024;

#[derive(Clone)]
pub struct Db {
    tx: mpsc::Sender<Job>,
    full: Arc<AtomicBool>,
}

/// Storage is near its quota once fewer pages than this are left: a
/// twentieth of the quota, at least a few pages. Writes already queued
/// can still be stored; new strokes and boards are refused.
fn reserve(max_pages: u64) -> u64 {
    (max_pages / 20).max(4)
}

/// Whether storage is near its quota. Pages on the free list are reused
/// before the file grows, so they count as room.
fn near_quota(conn: &Connection) -> bool {
    let get = |p: &str| conn.query_row(&format!("PRAGMA {p}"), [], |r| r.get::<_, i64>(0)).map(|v| v.max(0) as u64);
    match (get("max_page_count"), get("page_count"), get("freelist_count")) {
        (Ok(max), Ok(pages), Ok(free)) => pages.saturating_sub(free) + reserve(max) >= max,
        // Can't tell: fail closed.
        _ => true,
    }
}

pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS boards(
  id INTEGER PRIMARY KEY,
  edit_hash BLOB NOT NULL UNIQUE,
  view_hash BLOB NOT NULL UNIQUE,
  created INTEGER NOT NULL,
  touched INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS layers(
  board INTEGER NOT NULL,
  n INTEGER NOT NULL,
  who_hash BLOB NOT NULL,
  color TEXT NOT NULL,
  PRIMARY KEY(board, n)) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS strokes(
  board INTEGER NOT NULL,
  seq INTEGER NOT NULL,
  id TEXT NOT NULL,
  layer INTEGER NOT NULL,
  color TEXT NOT NULL,
  width REAL NOT NULL,
  pts TEXT NOT NULL,
  state INTEGER NOT NULL DEFAULT 0,
  txt TEXT,
  sheet INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY(board, seq)) WITHOUT ROWID;
CREATE UNIQUE INDEX IF NOT EXISTS strokes_id ON strokes(board, id);
";

impl Db {
    /// Open the database with the default quota.
    pub fn open(path: &str) -> rusqlite::Result<Db> {
        Db::open_with(path, MAX_DB)
    }

    /// Open the database, letting it grow to at most `max_bytes`. Past that
    /// a write fails rather than fill the disk; well before it, new strokes
    /// and boards are refused (see [`Db::is_full`]).
    pub fn open_with(path: &str, max_bytes: u64) -> rusqlite::Result<Db> {
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        // FULL: a commit is on disk before anything is acknowledged. NORMAL
        // can lose the last commits to a power cut, after their ok was sent.
        conn.pragma_update(None, "synchronous", "FULL")?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.execute_batch(SCHEMA)?;
        // Databases made before text annotations lack the column.
        let has_txt: bool = conn
            .prepare("SELECT 1 FROM pragma_table_info('strokes') WHERE name = 'txt'")?
            .exists([])?;
        if !has_txt {
            conn.execute_batch("ALTER TABLE strokes ADD COLUMN txt TEXT")?;
        }
        let has_sheet: bool = conn
            .prepare("SELECT 1 FROM pragma_table_info('strokes') WHERE name = 'sheet'")?
            .exists([])?;
        if !has_sheet {
            conn.execute_batch("ALTER TABLE strokes ADD COLUMN sheet INTEGER NOT NULL DEFAULT 0")?;
        }
        let page = conn.query_row("PRAGMA page_size", [], |r| r.get::<_, i64>(0))?.max(512) as u64;
        let max_pages = (max_bytes / page).max(1);
        let set = conn.query_row(&format!("PRAGMA max_page_count = {max_pages}"), [], |r| r.get::<_, i64>(0))?;
        if set as u64 != max_pages {
            // The file is already larger than the quota: SQLite keeps it.
            eprintln!("wb-relay: storage is past its quota already; new strokes will be refused");
        }
        let full = Arc::new(AtomicBool::new(near_quota(&conn)));
        let (tx, rx) = mpsc::channel();
        let flag = full.clone();
        std::thread::Builder::new()
            .name("wb-db".into())
            .spawn(move || run(conn, rx, flag))
            .expect("could not start the storage thread");
        Ok(Db { tx, full })
    }

    /// Storage is near its quota: refuse anything that would add to it.
    pub fn is_full(&self) -> bool {
        self.full.load(Ordering::Acquire)
    }

    fn send(&self, c: Cmd) {
        self.job(Job { cmd: c, link: None, ack: None });
    }

    fn write(&self, link: &Arc<Link>, cmd: Cmd, ack: Option<Ack>) {
        self.job(Job { cmd, link: Some(link.clone()), ack });
    }

    fn job(&self, j: Job) {
        if let Err(mpsc::SendError(j)) = self.tx.send(j) {
            eprintln!("wb-relay: storage thread is gone; a write was dropped");
            if let Some(link) = j.link {
                link.lose();
            }
        }
    }

    /// Run `ack` once everything this room queued before it is committed.
    pub fn ack(&self, link: &Arc<Link>, ack: Ack) {
        self.write(link, Cmd::Nop, Some(ack));
    }

    pub async fn create_board(&self, edit: [u8; 32], view: [u8; 32]) -> Option<i64> {
        let (reply, rx) = oneshot::channel();
        self.send(Cmd::CreateBoard { edit, view, reply });
        rx.await.ok().flatten()
    }

    /// Find the board a key opens, and mark the board as used now.
    pub async fn lookup(&self, hash: [u8; 32]) -> Option<(i64, Role)> {
        let (reply, rx) = oneshot::channel();
        self.send(Cmd::Lookup { hash, reply });
        rx.await.ok().flatten()
    }

    /// Read a board's layers and strokes. Err: they could not all be read
    /// (a storage error or a row that does not parse). The caller must not
    /// stand in an empty board for it: anything done on an empty stand-in
    /// (a crumple, a layer drop) would be written over the real rows.
    pub async fn load(&self, board: i64) -> Result<Loaded, ()> {
        let (reply, rx) = oneshot::channel();
        self.send(Cmd::Load { board, reply });
        rx.await.unwrap_or(Err(()))
    }

    pub fn add_layer(&self, link: &Arc<Link>, n: u32, who: [u8; 32], color: String) {
        self.write(link, Cmd::AddLayer { board: link.board, n, who, color }, None);
    }

    /// Store a stroke; `ack` runs once it is committed.
    #[allow(clippy::too_many_arguments)]
    pub fn add_stroke(
        &self,
        link: &Arc<Link>,
        seq: u64,
        id: String,
        layer: u32,
        color: String,
        width: f64,
        pts: String,
        txt: Option<String>,
        ack: Ack,
    ) {
        let board = link.board;
        self.write(link, Cmd::AddStroke { board, seq, id, layer, color, width, pts, txt }, Some(ack));
    }

    pub fn stroke_state(&self, link: &Arc<Link>, id: String, state: u8) {
        self.write(link, Cmd::StrokeState { board: link.board, id, state }, None);
    }

    pub fn layer_state(&self, link: &Arc<Link>, layer: u32, from: u8, to: u8) {
        self.write(link, Cmd::LayerState { board: link.board, layer, from, to }, None);
    }

    /// Move every stroke of one sheet to another sheet number (0 is the one in use).
    pub fn sheet_move(&self, link: &Arc<Link>, from: u32, to: u32) {
        self.write(link, Cmd::SheetMove { board: link.board, from, to }, None);
    }

    /// Throw a crumpled sheet away for good.
    pub fn sheet_drop(&self, link: &Arc<Link>, sheet: u32) {
        self.write(link, Cmd::SheetDrop { board: link.board, sheet }, None);
    }

    /// Delete boards not opened since `cutoff`, except those in `skip`.
    pub async fn sweep(&self, cutoff: i64, skip: Vec<i64>) -> usize {
        let (reply, rx) = oneshot::channel();
        self.send(Cmd::Sweep { cutoff, skip, reply });
        rx.await.unwrap_or(0)
    }

    /// Resolves once everything queued before it is committed.
    pub async fn flush(&self) {
        let (reply, rx) = oneshot::channel();
        self.send(Cmd::Flush { reply });
        let _ = rx.await;
    }
}

/// Runs after the transaction ends; true if it committed.
type After = Box<dyn FnOnce(bool)>;

fn run(conn: Connection, rx: mpsc::Receiver<Job>, full: Arc<AtomicBool>) {
    while let Ok(first) = rx.recv() {
        let mut after: Vec<After> = Vec::new();
        let mut writes: Vec<(Arc<Link>, Option<Ack>, bool)> = Vec::new();
        let mut committed = true;
        if let Err(e) = conn.execute_batch("BEGIN IMMEDIATE") {
            eprintln!("wb-relay: storage: begin failed: {e}");
            committed = false;
        }
        let mut next = Some(first);
        let mut n = 0;
        while let Some(job) = next.take() {
            let ok = match apply(&conn, job.cmd, &mut after) {
                Ok(()) => true,
                Err(e) => {
                    eprintln!("wb-relay: storage: {e}");
                    false
                }
            };
            if let Some(link) = job.link {
                writes.push((link, job.ack, ok));
            }
            n += 1;
            if n < 512 {
                next = rx.try_recv().ok();
            }
        }
        if committed {
            if let Err(e) = conn.execute_batch("COMMIT") {
                eprintln!("wb-relay: storage: commit failed: {e}");
                committed = false;
            }
        }
        if !committed {
            let _ = conn.execute_batch("ROLLBACK");
        }
        // Before the acks and replies: a client told ok and drawing again
        // must see the quota its own stroke reached.
        full.store(near_quota(&conn), Ordering::Release);
        // Every lost room first, so no ack goes out for a room that lost an
        // earlier write in the same batch.
        for (link, _, ok) in &writes {
            if !(committed && *ok) {
                link.lose();
            }
        }
        for (link, ack, _) in writes {
            if let Some(ack) = ack {
                if !link.is_dead() {
                    ack();
                }
            }
        }
        for f in after {
            f(committed);
        }
    }
}

fn load(conn: &Connection, board: i64) -> rusqlite::Result<Loaded> {
    let mut out = Loaded::default();
    let mut q = conn
        .prepare_cached("SELECT n, who_hash, color FROM layers WHERE board = ? ORDER BY n")?;
    let rows = q.query_map(params![board], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
    for row in rows {
        out.layers.push(row?);
    }
    let mut q = conn.prepare_cached(
        "SELECT seq, id, layer, color, width, pts, state, txt, sheet FROM strokes WHERE board = ? ORDER BY seq",
    )?;
    let rows = q.query_map(params![board], |r| {
        Ok(StrokeRow {
            seq: r.get::<_, i64>(0)? as u64,
            id: r.get(1)?,
            layer: r.get(2)?,
            color: r.get(3)?,
            width: r.get(4)?,
            pts: r.get(5)?,
            state: r.get(6)?,
            txt: r.get(7)?,
            sheet: r.get(8)?,
        })
    })?;
    for row in rows {
        out.strokes.push(row?);
    }
    Ok(out)
}

fn apply(
    conn: &Connection,
    cmd: Cmd,
    after: &mut Vec<After>,
) -> rusqlite::Result<()> {
    match cmd {
        Cmd::Nop => {}
        Cmd::CreateBoard { edit, view, reply } => {
            let t = now();
            let r = conn
                .prepare_cached(
                    "INSERT INTO boards(edit_hash, view_hash, created, touched) VALUES(?,?,?,?)",
                )?
                .execute(params![edit, view, t, t]);
            let id = r.ok().map(|_| conn.last_insert_rowid());
            after.push(Box::new(move |committed| {
                let _ = reply.send(id.filter(|_| committed));
            }));
        }
        Cmd::Lookup { hash, reply } => {
            let found: Option<(i64, bool)> = conn
                .prepare_cached(
                    "SELECT id, edit_hash = ?1 FROM boards WHERE edit_hash = ?1 OR view_hash = ?1",
                )?
                .query_row(params![hash], |r| Ok((r.get(0)?, r.get(1)?)))
                .ok();
            if let Some((id, _)) = found {
                conn.prepare_cached("UPDATE boards SET touched = ? WHERE id = ?")?
                    .execute(params![now(), id])?;
            }
            let out = found.map(|(id, edit)| (id, if edit { Role::Edit } else { Role::View }));
            after.push(Box::new(move |_| {
                let _ = reply.send(out);
            }));
        }
        Cmd::Load { board, reply } => {
            let r = load(conn, board);
            if let Err(e) = &r {
                eprintln!("wb-relay: storage: board {board} could not be loaded: {e}");
            }
            let _ = reply.send(r.map_err(|_| ()));
        }
        Cmd::AddLayer { board, n, who, color } => {
            conn.prepare_cached("INSERT INTO layers(board, n, who_hash, color) VALUES(?,?,?,?)")?
                .execute(params![board, n, who, color])?;
        }
        Cmd::AddStroke { board, seq, id, layer, color, width, pts, txt } => {
            conn.prepare_cached(
                "INSERT INTO strokes(board, seq, id, layer, color, width, pts, txt) VALUES(?,?,?,?,?,?,?,?)",
            )?
            .execute(params![board, seq as i64, id, layer, color, width, pts, txt])?;
        }
        Cmd::StrokeState { board, id, state } => {
            if state == UNDONE {
                // An undone stroke is never drawn again: keep the row for its
                // id, drop its points so undo can't be used to fill the disk.
                conn.prepare_cached(
                    "UPDATE strokes SET state = ?, pts = '[]', txt = NULL WHERE board = ? AND id = ?",
                )?
                .execute(params![state, board, id])?;
            } else {
                conn.prepare_cached("UPDATE strokes SET state = ? WHERE board = ? AND id = ?")?
                    .execute(params![state, board, id])?;
            }
        }
        Cmd::LayerState { board, layer, from, to } => {
            conn.prepare_cached(
                "UPDATE strokes SET state = ? WHERE board = ? AND layer = ? AND state = ? AND sheet = 0",
            )?
            .execute(params![to, board, layer, from])?;
        }
        Cmd::SheetMove { board, from, to } => {
            conn.prepare_cached("UPDATE strokes SET sheet = ? WHERE board = ? AND sheet = ?")?
                .execute(params![to, board, from])?;
        }
        Cmd::SheetDrop { board, sheet } => {
            conn.prepare_cached("DELETE FROM strokes WHERE board = ? AND sheet = ?")?
                .execute(params![board, sheet])?;
        }
        Cmd::Sweep { cutoff, skip, reply } => {
            let mut ids: Vec<i64> = Vec::new();
            {
                let mut q = conn.prepare_cached("SELECT id FROM boards WHERE touched < ?")?;
                let rows = q.query_map(params![cutoff], |r| r.get::<_, i64>(0))?;
                for row in rows {
                    let id = row?;
                    if !skip.contains(&id) {
                        ids.push(id);
                    }
                }
            }
            for id in &ids {
                conn.prepare_cached("DELETE FROM strokes WHERE board = ?")?.execute(params![id])?;
                conn.prepare_cached("DELETE FROM layers WHERE board = ?")?.execute(params![id])?;
                conn.prepare_cached("DELETE FROM boards WHERE id = ?")?.execute(params![id])?;
            }
            let n = ids.len();
            after.push(Box::new(move |committed| {
                let _ = reply.send(if committed { n } else { 0 });
            }));
        }
        Cmd::Flush { reply } => {
            after.push(Box::new(move |_| {
                let _ = reply.send(());
            }));
        }
    }
    Ok(())
}
