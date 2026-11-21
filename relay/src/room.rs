//! One board's state while anyone is connected to it, and the rules for every
//! message. All changes happen under one lock and are broadcast in that order,
//! so every client sees the same sequence.

use crate::db::{Db, Loaded, Role, DELETED, LIVE, UNDONE};
use crate::keys::{valid_color, valid_id};
use axum::extract::ws::Utf8Bytes;
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use tokio::sync::broadcast;

pub const ASPECT: f32 = 0.5625;
pub const MAX_MSG: usize = 1024 * 1024;
pub const MAX_POINTS: usize = 20_000;
pub const MAX_STROKES: usize = 10_000;
pub const MAX_LIVE_ROOM: usize = 64;
pub const MAX_LIVE_CONN: usize = 4;
pub const MAX_LAYERS: usize = 200;
pub const MAX_ROOM_CONNS: usize = 100;
pub const MAX_WIDTH: f32 = 0.2;
pub const MAX_TEXT_CHARS: usize = 500;
pub const MAX_TEXT_LINES: usize = 20;
/// Crumpled sheets kept per board. Crumpling one more throws the oldest away.
pub const MAX_SHELF: usize = 5;
const SNAPSHOT_CHUNK: usize = 200;

/// A message on its way to the board's connections.
#[derive(Clone)]
pub struct Out {
    /// Connection that must not receive it (0: nobody is skipped).
    pub skip: u64,
    /// The one connection that receives it (0: everyone).
    pub only: u64,
    pub text: Utf8Bytes,
}

#[derive(Deserialize)]
#[serde(tag = "t")]
pub enum In {
    #[serde(rename = "join")]
    Join { key: String, who: Option<String> },
    #[serde(rename = "b")]
    Begin { id: String, c: String, w: f32 },
    #[serde(rename = "p")]
    Points { id: String, pts: Vec<f32> },
    #[serde(rename = "e")]
    End {
        id: String,
        c: Option<String>,
        w: Option<f32>,
        pts: Option<Vec<f32>>,
    },
    #[serde(rename = "s")]
    Stroke {
        id: String,
        c: String,
        w: f32,
        pts: Vec<f32>,
        /// Present for a text annotation: `pts` is then its one anchor point
        /// and `w` its font size.
        txt: Option<String>,
    },
    #[serde(rename = "u")]
    Undo { id: String },
    #[serde(rename = "ld")]
    LayerDelete { l: u32 },
    #[serde(rename = "lr")]
    LayerRestore { l: u32 },
    /// Crumple the sheet in use: shelve everything on it and start blank.
    #[serde(rename = "cr")]
    Crumple {},
    /// Bring crumpled sheet `k` back; the sheet in use is crumpled in its place.
    #[serde(rename = "un")]
    Uncrumple { k: u32 },
    /// Ask for the strokes of crumpled sheet `k`, to draw a preview.
    #[serde(rename = "sv")]
    SheetView { k: u32 },
}

struct Stroke {
    seq: u64,
    layer: u32,
    state: u8,
    id: String,
    /// The stroke as it goes on the wire: {"id":..,"l":..,"q":..,"c":..,"w":..,"pts":[..]}
    json: String,
}

/// A crumpled sheet on the shelf.
struct Sheet {
    k: u32,
    strokes: Vec<Stroke>,
}

struct Layer {
    n: u32,
    who: [u8; 32],
    color: String,
}

struct Live {
    conn: u64,
    layer: u32,
    c: String,
    w: f32,
    pts: Vec<f32>,
}

struct State {
    next_seq: u64,
    /// Drawn and restorable strokes of the sheet in use, in sequence order.
    strokes: Vec<Stroke>,
    /// Crumpled sheets, oldest first.
    shelf: Vec<Sheet>,
    next_sheet: u32,
    /// Every stroke id the board has ever stored, undone ones included.
    ids: HashSet<String>,
    layers: Vec<Layer>,
    live: HashMap<String, Live>,
    conns: usize,
}

pub struct Room {
    pub board: i64,
    tx: broadcast::Sender<Out>,
    st: Mutex<State>,
}

/// What a connection is allowed to be on this board.
#[derive(Clone, Copy)]
pub struct Peer {
    pub conn: u64,
    pub role: Role,
    pub layer: u32,
}

pub struct Joined {
    pub peer: Peer,
    pub rx: broadcast::Receiver<Out>,
    /// hello, the snapshot chunks, live strokes and ready, in order.
    pub snapshot: Vec<Utf8Bytes>,
}

// ---- numbers on the wire ----------------------------------------------------

/// Write a value in 0..=1 rounded to 5 decimals, without trailing zeros.
fn push5(out: &mut String, v: f32) {
    let n = (f64::from(v) * 1e5).round().max(0.0) as u32;
    let (int, mut frac) = (n / 100_000, n % 100_000);
    out.push_str(&int.to_string());
    if frac == 0 {
        return;
    }
    out.push('.');
    let mut digits = [b'0'; 5];
    for d in digits.iter_mut().rev() {
        *d = b'0' + (frac % 10) as u8;
        frac /= 10;
    }
    let mut len = 5;
    while digits[len - 1] == b'0' {
        len -= 1;
    }
    for d in &digits[..len] {
        out.push(*d as char);
    }
}

fn pts_json(pts: &[f32]) -> String {
    let mut s = String::with_capacity(pts.len() * 8 + 2);
    s.push('[');
    for (i, v) in pts.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        push5(&mut s, *v);
    }
    s.push(']');
    s
}

fn width_json(w: f32) -> String {
    let mut s = String::new();
    push5(&mut s, w.max(0.00001));
    s
}

/// Check a point list and clamp it to the board. None: refuse the message.
fn clean_pts(mut v: Vec<f32>) -> Option<Vec<f32>> {
    if v.len() % 3 != 0 || v.len() > MAX_POINTS * 3 {
        return None;
    }
    for (i, x) in v.iter_mut().enumerate() {
        if !x.is_finite() {
            return None;
        }
        let hi = if i % 3 == 1 { ASPECT } else { 1.0 };
        *x = x.clamp(0.0, hi);
    }
    Some(v)
}

fn valid_width(w: f32) -> bool {
    w.is_finite() && w > 0.0 && w <= MAX_WIDTH
}

fn stroke_json(
    id: &str,
    layer: u32,
    seq: u64,
    c: &str,
    w: &str,
    pts: &str,
    txt: Option<&str>,
) -> String {
    match txt {
        None => format!(r#"{{"id":"{id}","l":{layer},"q":{seq},"c":"{c}","w":{w},"pts":{pts}}}"#),
        Some(t) => {
            let t = serde_json::to_string(t).expect("a string always serialises");
            format!(r#"{{"id":"{id}","l":{layer},"q":{seq},"c":"{c}","w":{w},"pts":{pts},"txt":{t}}}"#)
        }
    }
}

/// Check the text of an annotation. Returns it with line endings normalised
/// and trailing space removed, or None to refuse it.
fn clean_text(t: &str) -> Option<String> {
    let t = t.replace("\r\n", "\n").replace('\r', "\n");
    let t = t.trim_end();
    if t.trim().is_empty()
        || t.chars().count() > MAX_TEXT_CHARS
        || t.lines().count() > MAX_TEXT_LINES
        || t.chars().any(|c| c != '\n' && c.is_control())
    {
        return None;
    }
    Some(t.to_string())
}

/// Colour for the n-th contributor (n from 1). Ten fixed colours that read
/// well on white, then evenly spread hues. The first four match the page's
/// fixed swatches exactly, so the toolbar never shows a near-duplicate.
pub fn layer_color(n: u32) -> String {
    const FIXED: [&str; 10] = [
        "#111111", "#0a66d8", "#d63031", "#1e9e4a", "#8e44ad", "#e67e22", "#0f9d9d", "#c2185b",
        "#6d4c41", "#546e7a",
    ];
    if let Some(c) = FIXED.get(n as usize - 1) {
        return (*c).to_string();
    }
    let hue = (f64::from(n) * 137.508) % 360.0;
    let (s, l) = (0.7, 0.4);
    let c = (1.0 - (2.0 * l - 1.0f64).abs()) * s;
    let x = c * (1.0 - ((hue / 60.0) % 2.0 - 1.0).abs());
    let m = l - c / 2.0;
    let (r, g, b) = match (hue / 60.0) as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let to = |v: f64| ((v + m) * 255.0).round() as u8;
    format!("#{:02x}{:02x}{:02x}", to(r), to(g), to(b))
}

// ---- the room ---------------------------------------------------------------

impl Room {
    pub fn new(board: i64, loaded: Loaded) -> Room {
        let mut ids = HashSet::new();
        let mut strokes = Vec::new();
        let mut shelf: Vec<Sheet> = Vec::new();
        let mut next_seq = 1;
        let mut next_sheet = 1;
        for r in loaded.strokes {
            next_seq = next_seq.max(r.seq + 1);
            next_sheet = next_sheet.max(r.sheet + 1);
            if r.state != UNDONE {
                let w = width_json(r.width as f32);
                let into = if r.sheet == 0 {
                    &mut strokes
                } else {
                    let i = match shelf.iter().position(|s| s.k == r.sheet) {
                        Some(i) => i,
                        None => {
                            shelf.push(Sheet { k: r.sheet, strokes: Vec::new() });
                            shelf.len() - 1
                        }
                    };
                    &mut shelf[i].strokes
                };
                into.push(Stroke {
                    seq: r.seq,
                    layer: r.layer,
                    state: r.state,
                    json: stroke_json(&r.id, r.layer, r.seq, &r.color, &w, &r.pts, r.txt.as_deref()),
                    id: r.id.clone(),
                });
            }
            ids.insert(r.id);
        }
        shelf.sort_by_key(|s| s.k);
        let layers = loaded
            .layers
            .into_iter()
            .map(|(n, who, color)| Layer { n, who, color })
            .collect();
        let (tx, _) = broadcast::channel(1024);
        Room {
            board,
            tx,
            st: Mutex::new(State {
                next_seq,
                strokes,
                shelf,
                next_sheet,
                ids,
                layers,
                live: HashMap::new(),
                conns: 0,
            }),
        }
    }

    fn all(&self, text: String) {
        let _ = self.tx.send(Out { skip: 0, only: 0, text: text.into() });
    }
    fn others(&self, conn: u64, text: String) {
        let _ = self.tx.send(Out { skip: conn, only: 0, text: text.into() });
    }
    fn only(&self, conn: u64, text: String) {
        let _ = self.tx.send(Out { skip: 0, only: conn, text: text.into() });
    }

    pub fn conns(&self) -> usize {
        self.st.lock().unwrap().conns
    }

    /// Add a connection. `who` is the hash of the contributor secret (edit
    /// links only). `view` is the view key to hand to an editor.
    /// Err: the board has no room for another connection.
    pub fn join(
        &self,
        conn: u64,
        mut role: Role,
        who: Option<[u8; 32]>,
        view: Option<&str>,
        db: &Db,
    ) -> Result<Joined, ()> {
        let mut st = self.st.lock().unwrap();
        if st.conns >= MAX_ROOM_CONNS {
            return Err(());
        }
        let mut layer = 0;
        if role == Role::Edit {
            let who = who.ok_or(())?;
            if let Some(l) = st.layers.iter().find(|l| l.who == who) {
                layer = l.n;
            } else if st.layers.len() < MAX_LAYERS {
                layer = st.layers.len() as u32 + 1;
                let color = layer_color(layer);
                db.add_layer(self.board, layer, who, color.clone());
                self.all(format!(r#"{{"t":"l","l":{layer},"c":"{color}"}}"#));
                st.layers.push(Layer { n: layer, who, color });
            } else {
                role = Role::View;
            }
        }
        st.conns += 1;
        // Subscribe after the layer notice (the newcomer reads its layer from
        // hello) and before anything else can be broadcast.
        let rx = self.tx.subscribe();
        self.all(format!(r#"{{"t":"n","n":{}}}"#, st.conns));

        let mut snapshot = Vec::new();
        let mut hello = String::from(r#"{"t":"hello","role":"#);
        hello.push_str(if role == Role::Edit { r#""edit","me":"# } else { r#""view","me":"# });
        match role {
            Role::Edit => hello.push_str(&layer.to_string()),
            Role::View => hello.push_str("null"),
        }
        match (role, view) {
            (Role::Edit, Some(v)) => hello.push_str(&format!(r#","view":"{v}","layers":["#)),
            _ => hello.push_str(r#","view":null,"layers":["#),
        }
        hello.push_str(&layers_json(&st));
        hello.push_str(r#"],"shelf":"#);
        hello.push_str(&shelf_json(&st));
        hello.push('}');
        snapshot.push(hello.into());

        let drawn: Vec<&Stroke> = st.strokes.iter().filter(|s| s.state == LIVE).collect();
        for chunk in drawn.chunks(SNAPSHOT_CHUNK) {
            snapshot.push(strokes_msg(r#"{"t":"ss","a":["#, chunk.iter().map(|s| s.json.as_str())).into());
        }
        for (id, s) in &st.live {
            let w = width_json(s.w);
            snapshot.push(format!(r#"{{"t":"b","id":"{id}","l":{},"c":"{}","w":{w}}}"#, s.layer, s.c).into());
            if !s.pts.is_empty() {
                snapshot.push(format!(r#"{{"t":"p","id":"{id}","pts":{}}}"#, pts_json(&s.pts)).into());
            }
        }
        snapshot.push(Utf8Bytes::from_static(r#"{"t":"ready"}"#));
        Ok(Joined { peer: Peer { conn, role, layer }, rx, snapshot })
    }

    /// Remove a connection and abandon the strokes it left unfinished.
    pub fn leave(&self, conn: u64) {
        let mut st = self.st.lock().unwrap();
        st.conns = st.conns.saturating_sub(1);
        let gone: Vec<String> =
            st.live.iter().filter(|(_, s)| s.conn == conn).map(|(id, _)| id.clone()).collect();
        for id in gone {
            st.live.remove(&id);
            self.others(conn, format!(r#"{{"t":"u","id":"{id}"}}"#));
        }
        self.all(format!(r#"{{"t":"n","n":{}}}"#, st.conns));
    }

    /// Apply one message from a connection that has joined.
    pub fn handle(&self, peer: Peer, text: &str, db: &Db) {
        if peer.role != Role::Edit {
            return; // viewers can't change the board
        }
        let Ok(msg) = serde_json::from_str::<In>(text) else {
            return;
        };
        let mut st = self.st.lock().unwrap();
        let conn = peer.conn;
        match msg {
            In::Join { .. } => {}
            In::Begin { id, c, w } => {
                if !valid_id(&id) || !valid_color(&c) || !valid_width(w) {
                    return;
                }
                if st.ids.contains(&id) || st.live.contains_key(&id) {
                    return;
                }
                if st.live.len() >= MAX_LIVE_ROOM
                    || st.live.values().filter(|s| s.conn == conn).count() >= MAX_LIVE_CONN
                {
                    return;
                }
                let wj = width_json(w);
                self.others(
                    conn,
                    format!(r#"{{"t":"b","id":"{id}","l":{},"c":"{c}","w":{wj}}}"#, peer.layer),
                );
                st.live.insert(id, Live { conn, layer: peer.layer, c, w, pts: Vec::new() });
            }
            In::Points { id, pts } => {
                let Some(s) = st.live.get_mut(&id) else { return };
                if s.conn != conn {
                    return;
                }
                let Some(pts) = clean_pts(pts) else { return };
                if s.pts.len() + pts.len() > MAX_POINTS * 3 {
                    return;
                }
                self.others(conn, format!(r#"{{"t":"p","id":"{id}","pts":{}}}"#, pts_json(&pts)));
                s.pts.extend_from_slice(&pts);
            }
            In::End { id, c, w, pts } => {
                if !valid_id(&id) {
                    return;
                }
                let live = match st.live.get(&id) {
                    Some(s) if s.conn == conn => st.live.remove(&id),
                    Some(_) => return, // someone else's live stroke
                    None => None,
                };
                let given = pts.and_then(clean_pts).filter(|p| !p.is_empty());
                let (c, w, pts, was_live) = match live {
                    Some(s) => (s.c, s.w, given.unwrap_or(s.pts), true),
                    None => match (c, w, given) {
                        (Some(c), Some(w), Some(p)) if valid_color(&c) && valid_width(w) => {
                            (c, w, p, false)
                        }
                        _ => {
                            self.only(conn, refusal(&id, "the stroke was incomplete"));
                            return;
                        }
                    },
                };
                self.commit(&mut st, peer, id, c, w, pts, None, was_live, db);
            }
            In::Stroke { id, c, w, pts, txt } => {
                if !valid_id(&id) {
                    return;
                }
                let was_live = match st.live.get(&id) {
                    Some(s) if s.conn == conn => {
                        st.live.remove(&id);
                        true
                    }
                    Some(_) => return,
                    None => false,
                };
                let pts = clean_pts(pts).filter(|p| !p.is_empty());
                // A text annotation has exactly one point and good text.
                let txt = match txt {
                    None => Ok(None),
                    Some(t) => match (clean_text(&t), &pts) {
                        (Some(t), Some(p)) if p.len() == 3 => Ok(Some(t)),
                        _ => Err(()),
                    },
                };
                match (pts, txt) {
                    (Some(pts), Ok(txt)) if valid_color(&c) && valid_width(w) => {
                        self.commit(&mut st, peer, id, c, w, pts, txt, was_live, db);
                    }
                    _ => {
                        if was_live {
                            self.others(conn, format!(r#"{{"t":"u","id":"{id}"}}"#));
                        }
                        self.only(conn, refusal(&id, "the stroke was not valid"));
                    }
                }
            }
            In::Undo { id } => {
                if !valid_id(&id) {
                    return;
                }
                if st.live.get(&id).is_some_and(|s| s.conn == conn) {
                    st.live.remove(&id);
                    self.others(conn, format!(r#"{{"t":"u","id":"{id}"}}"#));
                    return;
                }
                let Some(i) = st
                    .strokes
                    .iter()
                    .rposition(|s| s.id == id && s.layer == peer.layer && s.state == LIVE)
                else {
                    return;
                };
                st.strokes.remove(i);
                db.stroke_state(self.board, id.clone(), UNDONE);
                self.others(conn, format!(r#"{{"t":"u","id":"{id}"}}"#));
            }
            In::LayerDelete { l } => {
                let mut any = false;
                for s in st.strokes.iter_mut().filter(|s| s.layer == l && s.state == LIVE) {
                    s.state = DELETED;
                    any = true;
                }
                if any {
                    db.layer_state(self.board, l, LIVE, DELETED);
                    self.all(format!(r#"{{"t":"ld","l":{l}}}"#));
                }
            }
            In::Crumple {} => {
                if st.strokes.is_empty() {
                    return;
                }
                let k = st.next_sheet;
                st.next_sheet += 1;
                let sheet = std::mem::take(&mut st.strokes);
                db.sheet_move(self.board, 0, k);
                st.shelf.push(Sheet { k, strokes: sheet });
                let mut dropped = String::from("null");
                if st.shelf.len() > MAX_SHELF {
                    let old = st.shelf.remove(0);
                    for s in &old.strokes {
                        st.ids.remove(&s.id);
                    }
                    db.sheet_drop(self.board, old.k);
                    dropped = old.k.to_string();
                }
                self.all(format!(
                    r#"{{"t":"cr","k":{k},"by":{},"drop":{dropped},"shelf":{}}}"#,
                    peer.layer,
                    shelf_json(&st)
                ));
            }
            In::Uncrumple { k } => {
                let Some(i) = st.shelf.iter().position(|s| s.k == k) else { return };
                let back = st.shelf.remove(i);
                let cur = std::mem::replace(&mut st.strokes, back.strokes);
                // Storage: first move the sheet in use out of the way, then bring k in.
                let nk = st.next_sheet;
                st.next_sheet += 1;
                db.sheet_move(self.board, 0, nk);
                db.sheet_move(self.board, k, 0);
                if !cur.is_empty() {
                    st.shelf.push(Sheet { k: nk, strokes: cur });
                }
                let head = format!(
                    r#"{{"t":"un","k":{k},"by":{},"shelf":{},"layers":[{}],"a":["#,
                    peer.layer,
                    shelf_json(&st),
                    layers_json(&st)
                );
                let drawn = st.strokes.iter().filter(|s| s.state == LIVE).map(|s| s.json.as_str());
                self.all(strokes_msg(&head, drawn));
            }
            In::SheetView { k } => {
                let Some(sheet) = st.shelf.iter().find(|s| s.k == k) else { return };
                let head = format!(r#"{{"t":"sv","k":{k},"a":["#);
                let drawn = sheet.strokes.iter().filter(|s| s.state == LIVE).map(|s| s.json.as_str());
                self.only(conn, strokes_msg(&head, drawn));
            }
            In::LayerRestore { l } => {
                let mut back: Vec<&str> = Vec::new();
                for s in st.strokes.iter_mut().filter(|s| s.layer == l && s.state == DELETED) {
                    s.state = LIVE;
                    back.push(&s.json);
                }
                if !back.is_empty() {
                    db.layer_state(self.board, l, DELETED, LIVE);
                    let head = format!(r#"{{"t":"lr","l":{l},"a":["#);
                    self.all(strokes_msg(&head, back.into_iter()));
                }
            }
        }
    }

    /// Store a finished stroke and tell everyone.
    #[allow(clippy::too_many_arguments)]
    fn commit(
        &self,
        st: &mut State,
        peer: Peer,
        id: String,
        c: String,
        w: f32,
        pts: Vec<f32>,
        txt: Option<String>,
        was_live: bool,
        db: &Db,
    ) {
        let conn = peer.conn;
        let drop_live = |room: &Room| {
            if was_live {
                room.others(conn, format!(r#"{{"t":"u","id":"{id}"}}"#));
            }
        };
        if pts.is_empty() {
            drop_live(self);
            self.only(conn, refusal(&id, "the stroke was empty"));
            return;
        }
        if st.ids.contains(&id) {
            // A resend. Confirm it again if it is still this layer's stroke.
            drop_live(self);
            match st.strokes.iter().rev().find(|s| s.id == id) {
                Some(s) if s.layer == peer.layer => {
                    self.only(conn, format!(r#"{{"t":"ok","id":"{id}","q":{}}}"#, s.seq));
                }
                _ => self.only(conn, format!(r#"{{"t":"u","id":"{id}"}}"#)),
            }
            return;
        }
        if st.strokes.len() + st.shelf.iter().map(|s| s.strokes.len()).sum::<usize>() >= MAX_STROKES {
            drop_live(self);
            self.only(conn, refusal(&id, "this board is full"));
            return;
        }
        let seq = st.next_seq;
        st.next_seq += 1;
        let (wj, pj) = (width_json(w), pts_json(&pts));
        let json = stroke_json(&id, peer.layer, seq, &c, &wj, &pj, txt.as_deref());
        self.others(conn, format!(r#"{{"t":"e",{}"#, &json[1..]));
        self.only(conn, format!(r#"{{"t":"ok","id":"{id}","q":{seq}}}"#));
        db.add_stroke(self.board, seq, id.clone(), peer.layer, c, f64::from(w), pj, txt);
        st.ids.insert(id.clone());
        st.strokes.push(Stroke { seq, layer: peer.layer, state: LIVE, id, json });
    }
}

/// The layers as they go in hello and un: `{l, c, del}` objects, comma separated.
fn layers_json(st: &State) -> String {
    let mut out = String::new();
    for (i, l) in st.layers.iter().enumerate() {
        let del = st.strokes.iter().any(|s| s.layer == l.n && s.state == DELETED);
        if i > 0 {
            out.push(',');
        }
        out.push_str(&format!(r#"{{"l":{},"c":"{}","del":{del}}}"#, l.n, l.color));
    }
    out
}

/// The shelf, oldest first: `[{k, n}]` with n the number of drawn items.
fn shelf_json(st: &State) -> String {
    let mut out = String::from("[");
    for (i, s) in st.shelf.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        let n = s.strokes.iter().filter(|x| x.state == LIVE).count();
        out.push_str(&format!(r#"{{"k":{},"n":{n}}}"#, s.k));
    }
    out.push(']');
    out
}

fn refusal(id: &str, why: &str) -> String {
    format!(r#"{{"t":"no","id":"{id}","m":"{why}"}}"#)
}

fn strokes_msg<'a>(head: &str, items: impl Iterator<Item = &'a str>) -> String {
    let mut s = String::from(head);
    for (i, j) in items.enumerate() {
        if i > 0 {
            s.push(',');
        }
        s.push_str(j);
    }
    s.push_str("]}");
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_round_to_five_places() {
        let j = pts_json(&[0.0, 0.5625, 1.0, 0.123456, 0.1, 0.000004]);
        assert_eq!(j, "[0,0.5625,1,0.12346,0.1,0]");
        assert_eq!(width_json(0.004), "0.004");
        assert_eq!(width_json(0.0000001), "0.00001");
    }

    #[test]
    fn points_are_clamped_and_checked() {
        assert_eq!(clean_pts(vec![-1.0, 2.0, 5.0]).unwrap(), vec![0.0, ASPECT, 1.0]);
        assert!(clean_pts(vec![0.1, 0.1]).is_none());
        assert!(clean_pts(vec![0.1, f32::NAN, 0.5]).is_none());
        assert!(clean_pts(vec![0.1, f32::INFINITY, 0.5]).is_none());
        assert!(clean_pts(vec![0.0; (MAX_POINTS + 1) * 3]).is_none());
        assert!(clean_pts(vec![0.0; MAX_POINTS * 3]).is_some());
    }

    #[test]
    fn text_is_checked() {
        assert_eq!(clean_text("hi\r\nthere  ").unwrap(), "hi\nthere");
        assert!(clean_text("   \n ").is_none());
        assert!(clean_text("bell\u{7}").is_none());
        assert!(clean_text(&"x".repeat(MAX_TEXT_CHARS + 1)).is_none());
        assert!(clean_text(&"x\n".repeat(MAX_TEXT_LINES + 1)).is_none());
        assert!(clean_text("naïve “quotes” \\ \" 日本語").is_some());
        let j = stroke_json("a", 1, 2, "#111111", "0.02", "[0.1,0.2,0.5]", Some("say \"hi\"\n</script>"));
        let v: serde_json::Value = serde_json::from_str(&j).unwrap();
        assert_eq!(v["txt"], "say \"hi\"\n</script>");
    }

    #[test]
    fn layer_colours_are_valid_and_start_dark() {
        assert_eq!(layer_color(1), "#111111");
        for n in 1..=MAX_LAYERS as u32 {
            assert!(valid_color(&layer_color(n)), "layer {n}");
        }
        assert_ne!(layer_color(11), layer_color(12));
    }
}
