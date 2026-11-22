//! SQLite storage. One thread owns the connection and takes commands in order,
//! so the relay's async tasks never block on disk and every read sees every
//! write queued before it. Commands that arrive together share a transaction.

use rusqlite::{params, Connection};
use std::sync::mpsc;
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

enum Cmd {
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

#[derive(Clone)]
pub struct Db {
    tx: mpsc::Sender<Cmd>,
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
    pub fn open(path: &str) -> rusqlite::Result<Db> {
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
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
        let (tx, rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("wb-db".into())
            .spawn(move || run(conn, rx))
            .expect("could not start the storage thread");
        Ok(Db { tx })
    }

    fn send(&self, c: Cmd) {
        if self.tx.send(c).is_err() {
            eprintln!("wb-relay: storage thread is gone; a write was dropped");
        }
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

    pub fn add_layer(&self, board: i64, n: u32, who: [u8; 32], color: String) {
        self.send(Cmd::AddLayer { board, n, who, color });
    }

    #[allow(clippy::too_many_arguments)]
    pub fn add_stroke(
        &self,
        board: i64,
        seq: u64,
        id: String,
        layer: u32,
        color: String,
        width: f64,
        pts: String,
        txt: Option<String>,
    ) {
        self.send(Cmd::AddStroke { board, seq, id, layer, color, width, pts, txt });
    }

    pub fn stroke_state(&self, board: i64, id: String, state: u8) {
        self.send(Cmd::StrokeState { board, id, state });
    }

    pub fn layer_state(&self, board: i64, layer: u32, from: u8, to: u8) {
        self.send(Cmd::LayerState { board, layer, from, to });
    }

    /// Move every stroke of one sheet to another sheet number (0 is the one in use).
    pub fn sheet_move(&self, board: i64, from: u32, to: u32) {
        self.send(Cmd::SheetMove { board, from, to });
    }

    /// Throw a crumpled sheet away for good.
    pub fn sheet_drop(&self, board: i64, sheet: u32) {
        self.send(Cmd::SheetDrop { board, sheet });
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

fn run(conn: Connection, rx: mpsc::Receiver<Cmd>) {
    while let Ok(first) = rx.recv() {
        let mut after: Vec<Box<dyn FnOnce()>> = Vec::new();
        if let Err(e) = conn.execute_batch("BEGIN IMMEDIATE") {
            eprintln!("wb-relay: storage: begin failed: {e}");
        }
        let mut next = Some(first);
        let mut n = 0;
        while let Some(cmd) = next.take() {
            if let Err(e) = apply(&conn, cmd, &mut after) {
                eprintln!("wb-relay: storage: {e}");
            }
            n += 1;
            if n < 512 {
                next = rx.try_recv().ok();
            }
        }
        if let Err(e) = conn.execute_batch("COMMIT") {
            eprintln!("wb-relay: storage: commit failed: {e}");
            let _ = conn.execute_batch("ROLLBACK");
        }
        for f in after {
            f();
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
    after: &mut Vec<Box<dyn FnOnce()>>,
) -> rusqlite::Result<()> {
    match cmd {
        Cmd::CreateBoard { edit, view, reply } => {
            let t = now();
            let r = conn
                .prepare_cached(
                    "INSERT INTO boards(edit_hash, view_hash, created, touched) VALUES(?,?,?,?)",
                )?
                .execute(params![edit, view, t, t]);
            let id = r.ok().map(|_| conn.last_insert_rowid());
            after.push(Box::new(move || {
                let _ = reply.send(id);
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
            after.push(Box::new(move || {
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
            after.push(Box::new(move || {
                let _ = reply.send(n);
            }));
        }
        Cmd::Flush { reply } => {
            after.push(Box::new(move || {
                let _ = reply.send(());
            }));
        }
    }
    Ok(())
}
