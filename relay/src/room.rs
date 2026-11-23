//! One board's state while anyone is connected to it, and the rules for every
//! message. All changes happen under one lock and are broadcast in that order,
//! so every client sees the same sequence.

use crate::db::{Db, Link, Loaded, Role, DELETED, LIVE, UNDONE};
use crate::keys::{valid_color, valid_id};
use crate::limits::{Bucket, Budget, SHEETS_BURST, SHEETS_PER_SEC};
use crate::memory::{weight, Held, Memory};
use axum::extract::ws::Utf8Bytes;
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::broadcast;

pub const ASPECT: f32 = 0.5625;
pub const MAX_MSG: usize = 1024 * 1024;
pub const MAX_POINTS: usize = 20_000;
pub const MAX_STROKES: usize = 10_000;
/// Stored stroke data per board, sheet in use plus shelf. Bounds memory and
/// disk whatever the stroke count: one stroke can be close to half a megabyte.
pub const MAX_BOARD_BYTES: usize = 16 * 1024 * 1024;
/// Stroke ids a board remembers, undone ones included.
pub const MAX_IDS: usize = 50_000;
pub const MAX_LIVE_ROOM: usize = 64;
pub const MAX_LIVE_CONN: usize = 4;
pub const MAX_LAYERS: usize = 200;
pub const MAX_ROOM_CONNS: usize = 100;
/// Viewers may fill this many of a board's connections; the rest are kept
/// for contributors, so a few addresses holding view sockets can't lock the
/// editors out.
pub const MAX_ROOM_VIEWERS: usize = 80;
pub const MAX_WIDTH: f32 = 0.2;
pub const MAX_TEXT_CHARS: usize = 500;
pub const MAX_TEXT_LINES: usize = 20;
/// Crumpled sheets kept per board. Crumpling one more throws the oldest away.
pub const MAX_SHELF: usize = 5;
/// An attention ping is a rough circle, not a drawing.
const MAX_PING_POINTS: usize = 300;
const PING_EVERY: Duration = Duration::from_secs(2);
const SNAPSHOT_CHUNK: usize = 200;

/// A message on its way to the board's connections.
#[derive(Clone)]
pub struct Out {
    /// Connection that must not receive it (0: nobody is skipped).
    pub skip: u64,
    /// The one connection that receives it (0: everyone).
    pub only: u64,
    pub text: Utf8Bytes,
    /// The room is lost (a write failed): every connection ends with busy.
    pub end: bool,
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
        /// Present for a shape or an arrow: checked by [`clean_shape`].
        sh: Option<serde_json::Value>,
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
    /// My label: a display name and my colour. Kept in memory only; the
    /// browser sends it again on every join.
    #[serde(rename = "nm")]
    Name { n: String, c: String },
    /// An attention ping: a shape shown to everyone for a few seconds and
    /// never stored.
    #[serde(rename = "pg")]
    Ping { pts: Vec<f32> },
    /// Several `s` and `u` from one edit (a move of a selection, a group, an ungroup),
    /// applied together or not at all, and undone in one step by the page.
    #[serde(rename = "x")]
    Batch { a: Vec<serde_json::Value> },
}

struct Stroke {
    seq: u64,
    layer: u32,
    state: u8,
    id: String,
    /// The stroke as it goes on the wire: {"id":..,"l":..,"q":..,"c":..,"w":..,"pts":[..]}
    json: String,
    /// A shape's node name: arrows attach to it, so only one layer may hold it (S1).
    node: Option<String>,
}

/// A crumpled sheet on the shelf.
struct Sheet {
    k: u32,
    strokes: Vec<Stroke>,
    /// The `sv` preview message, built on first request. A shelved sheet
    /// never changes, so it is built once and shared.
    view: Option<Utf8Bytes>,
}

struct Layer {
    n: u32,
    who: [u8; 32],
    color: String,
    name: Option<String>,
    /// Connections open on this layer now.
    on: u32,
    pinged: Option<Instant>,
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
    /// Bytes of stroke JSON held, sheet in use plus shelf.
    bytes: usize,
    /// What the shared memory budget holds for this board; 0 until admitted.
    charged: usize,
    /// The snapshot's `ss` chunks, shared by every join until the sheet changes.
    snap: Option<Arc<Snap>>,
    /// Every stroke id the board has ever stored, undone ones included.
    ids: HashSet<String>,
    layers: Vec<Layer>,
    live: HashMap<String, Live>,
    conns: usize,
    /// The connections that joined as viewers (counted in `conns` too).
    viewers: usize,
    /// Bytes of whole sheets this board may still broadcast (`un`, `lr`).
    sheets: Bucket,
    /// The next ping's letter, 0 for A, wrapping after Z.
    next_ping: u8,
}

/// One build of the sheet's `ss` chunks, shared by the cache and by every join still
/// sending it (council H4). A change to the sheet drops the cache, but a slow join keeps
/// its build alive, and the next join makes a new one. The room's weight counts one build
/// ([`crate::memory::COPIES`]); every other build alive at the same time is charged to the
/// shared memory budget until its last holder lets go, so slow joins cannot pin
/// uncounted copies of a big sheet.
pub struct Snap {
    chunks: Vec<Utf8Bytes>,
    /// This build is the one the room's weight counts (`spare` was free when it was made).
    counted: bool,
    /// Bytes charged to the budget for this build; 0 when `counted`.
    extra: usize,
    spare: Arc<AtomicBool>,
    mem: Arc<Memory>,
}

impl Drop for Snap {
    fn drop(&mut self) {
        if self.counted {
            self.spare.store(false, Ordering::SeqCst);
        } else {
            self.mem.shrink(self.extra);
        }
    }
}

/// The bytes of one join's copy of the live strokes, charged to the shared memory budget
/// until the join has sent them (council H4: 64 live strokes of 20k points are some 33MB
/// a join, and a slow join keeps its copy for minutes).
///
/// The address that joined is charged too, for these bytes and for an extra build of the
/// snapshot it holds, so one address cannot hold the whole budget with slow joins (P3-B).
pub struct Charge {
    bytes: usize,
    mem: Arc<Memory>,
    addr: Option<(Held, usize)>,
}

impl Drop for Charge {
    fn drop(&mut self) {
        self.mem.shrink(self.bytes);
        if let Some((h, n)) = &self.addr {
            h.give(*n);
        }
    }
}

/// Why a join was refused.
#[derive(Debug, PartialEq)]
pub enum NoJoin {
    /// No seat for it.
    Full,
    /// The memory budget has no room for another build of the snapshot.
    Busy,
}

pub struct Room {
    pub board: i64,
    tx: broadcast::Sender<Out>,
    link: Arc<Link>,
    mem: Arc<Memory>,
    /// Set while a build of the snapshot is alive that the room's weight counts.
    spare: Arc<AtomicBool>,
    st: Mutex<State>,
}

impl Drop for Room {
    fn drop(&mut self) {
        let st = self.st.get_mut().unwrap_or_else(|e| e.into_inner());
        if st.charged > 0 {
            self.mem.release(st.charged);
        }
    }
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
    /// The build of the chunks in `snapshot`, held until they are sent so its charge lasts
    /// as long as the copy does.
    pub hold: Option<Arc<Snap>>,
    /// The charge for the live strokes in `snapshot`, held until they are sent.
    pub live: Option<Charge>,
}

// ---- numbers on the wire ----------------------------------------------------

/// Write a small value (a few units either side of zero) rounded to 5
/// decimals, without trailing zeros.
fn push5(out: &mut String, v: f32) {
    let r = (f64::from(v) * 1e5).round();
    if r < 0.0 {
        out.push('-');
    }
    let n = r.abs() as u32;
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
        // The surface is a 3 by 3 grid of pages; the centre page is x 0..1, y 0..ASPECT.
        let (lo, hi) = match i % 3 {
            0 => (-1.0, 2.0),
            1 => (-ASPECT, 2.0 * ASPECT),
            _ => (0.0, 1.0),
        };
        *x = x.clamp(lo, hi);
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
    sh: Option<&str>,
) -> String {
    let mut s = format!(r#"{{"id":"{id}","l":{layer},"q":{seq},"c":"{c}","w":{w},"pts":{pts}"#);
    if let Some(t) = txt {
        s.push_str(r#","txt":"#);
        s.push_str(&serde_json::to_string(t).expect("a string always serialises"));
    }
    if let Some(sh) = sh {
        // Already canonical JSON, written by `clean_shape`.
        s.push_str(r#","sh":"#);
        s.push_str(sh);
    }
    s.push('}');
    s
}

/// Deepest nesting of groups an item can be in.
pub const MAX_GROUP_DEPTH: usize = 8;
/// Most changes one batch (`x`) may carry.
pub const MAX_BATCH: usize = 256;

/// Check the `sh` of an item and write it back out in its one canonical form, or None to
/// refuse it. `txt` is whether the item is a text annotation.
///
/// - A shape (`rect`, `ellipse`, `tri`): its node name `n`, whether it is filled `f`, and
///   optionally a fill colour `fc` (the paint bucket) and a dash style `d`.
/// - An arrow (`line`): its heads `h`, the shapes it joins `a` and `b`, a dash style `d`.
/// - A text annotation (`text`): bold `b` and italic `i`.
/// - A freehand stroke (`ink`): only there to carry its groups.
///
/// Any of them may carry `g`, the groups it is in from the outermost in. Shapes and arrows
/// have exactly two points (a shape the corners of its box, an arrow where its ends were
/// drawn); text has its one anchor point. No other keys are allowed, so nothing unchecked is
/// stored or sent on.
pub(crate) fn clean_shape(v: &serde_json::Value, pts: &[f32], txt: bool) -> Option<String> {
    let m = v.as_object()?;
    let k = m.get("k")?.as_str()?;
    let allowed: &[&str] = match k {
        "rect" | "ellipse" | "tri" => &["k", "n", "f", "fc", "d", "g"],
        "line" => &["k", "h", "a", "b", "d", "g"],
        "text" => &["k", "b", "i", "g"],
        "ink" => &["k", "g"],
        _ => return None,
    };
    if m.keys().any(|key| !allowed.contains(&key.as_str())) {
        return None;
    }
    match k {
        "text" if !txt || pts.len() != 3 => return None,
        "text" => {}
        _ if txt => return None,
        "ink" if pts.is_empty() => return None,
        "ink" => {}
        _ if pts.len() != 6 => return None,
        _ => {}
    }
    let flag = |key: &str| -> Option<bool> {
        match m.get(key) {
            None => Some(false),
            Some(f) => f.as_bool(),
        }
    };
    let name = |k: &str| m.get(k).and_then(|x| x.as_str()).filter(|s| valid_id(s));
    let mut out = format!(r#"{{"k":"{k}""#);
    match k {
        "rect" | "ellipse" | "tri" => {
            let n = name("n")?;
            out += &format!(r#","n":"{n}","f":{}"#, flag("f")?);
            if let Some(fc) = m.get("fc") {
                let fc = fc.as_str().filter(|c| valid_color(c))?;
                out += &format!(r#","fc":"{}""#, fc.to_ascii_lowercase());
            }
        }
        "line" => {
            let h = match m.get("h") {
                None => "end",
                Some(h) => h.as_str().filter(|h| matches!(*h, "none" | "end" | "start" | "both"))?,
            };
            out += &format!(r#","h":"{h}""#);
            // An end is free (absent: the drawn point in pts) or attached to a node at an
            // anchor: side s (0..=3) and t (0..=1) along it, both relative to the shape.
            let end = |k: &str| -> Option<Option<(String, String)>> {
                let Some(e) = m.get(k) else { return Some(None) };
                let e = e.as_object()?;
                if e.keys().any(|key| !matches!(key.as_str(), "n" | "s" | "t")) {
                    return None;
                }
                let n = e.get("n")?.as_str().filter(|n| valid_id(n))?;
                let side = e.get("s")?.as_u64().filter(|s| *s <= 3)?;
                let t = e.get("t")?.as_f64().filter(|t| t.is_finite() && (0.0..=1.0).contains(t))?;
                // + 0.0 turns -0 into 0, so it is never stored as "-0"
                let t = (t * 10000.0).round() / 10000.0 + 0.0;
                Some(Some((n.to_string(), format!(r#"{{"n":"{n}","s":{side},"t":{t}}}"#))))
            };
            let (a, b) = (end("a")?, end("b")?);
            if let (Some((na, _)), Some((nb, _))) = (&a, &b) {
                if na == nb {
                    return None;
                }
            }
            for (k, e) in [("a", a), ("b", b)] {
                if let Some((_, j)) = e {
                    out += &format!(r#","{k}":{j}"#);
                }
            }
        }
        "text" => {
            for key in ["b", "i"] {
                if flag(key)? {
                    out += &format!(r#","{key}":true"#);
                }
            }
        }
        _ => {}
    }
    if let Some(d) = m.get("d") {
        let d = d.as_str().filter(|d| matches!(*d, "dash" | "dot"))?;
        out += &format!(r#","d":"{d}""#);
    }
    if let Some(g) = m.get("g") {
        let g = g.as_array()?;
        if g.is_empty() || g.len() > MAX_GROUP_DEPTH {
            return None;
        }
        let mut seen = HashSet::new();
        let mut ids = Vec::new();
        for x in g {
            let x = x.as_str().filter(|x| valid_id(x))?;
            if !seen.insert(x) {
                return None;
            }
            ids.push(format!(r#""{x}""#));
        }
        out += &format!(r#","g":[{}]"#, ids.join(","));
    } else if k == "ink" {
        return None; // a freehand stroke has an `sh` only for its groups
    }
    Some(out + "}")
}

/// The node name of a shape, read from its canonical `sh`. None for anything else.
fn box_node(sh: &str) -> Option<&str> {
    let rest = ["rect", "ellipse", "tri"].iter().find_map(|k| sh.strip_prefix(&format!(r#"{{"k":"{k}","n":""#)))?;
    // canonical: the name is a valid id, so it holds no quote
    rest.split('"').next()
}

/// Whether a live shape on another layer than `layer` holds node name `n` (S1): a peer must
/// not take over the arrows attached to someone else's shape.
fn node_taken(st: &State, n: &str, layer: u32) -> bool {
    st.strokes.iter().any(|s| s.state == LIVE && s.layer != layer && s.node.as_deref() == Some(n))
}

/// A finished item's fields checked, or None to refuse it: a good colour and width, points,
/// text with exactly one point, and an `sh` in its canonical form.
#[allow(clippy::type_complexity)]
fn clean_item(
    c: String,
    w: f32,
    pts: Vec<f32>,
    txt: Option<String>,
    sh: Option<serde_json::Value>,
) -> Option<(String, f32, Vec<f32>, Option<String>, Option<String>)> {
    if !valid_color(&c) || !valid_width(w) {
        return None;
    }
    let pts = clean_pts(pts).filter(|p| !p.is_empty())?;
    let txt = match txt {
        None => None,
        Some(t) if pts.len() == 3 => Some(clean_text(&t)?),
        Some(_) => return None,
    };
    let sh = match sh {
        None => None,
        Some(v) => Some(clean_shape(&v, &pts, txt.is_some())?),
    };
    Some((c, w, pts, txt, sh))
}

/// Check the text of an annotation. Returns it with line endings normalised
/// and trailing space removed, or None to refuse it.
/// Characters taken out of names and text: bidi controls, which can make a
/// name read as something else or flip the text beside it, and invisible
/// format characters. The zero-width joiner and non-joiner stay: emoji
/// sequences and scripts such as Persian need them.
fn invisible(c: char) -> bool {
    matches!(c,
        '\u{00AD}' | '\u{061C}' | '\u{180E}' | '\u{200B}' | '\u{200E}' | '\u{200F}'
        | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{2064}' | '\u{2066}'..='\u{206F}'
        | '\u{FEFF}' | '\u{FFF9}'..='\u{FFFB}')
}

fn clean_text(t: &str) -> Option<String> {
    let t: String = t.chars().filter(|&c| !invisible(c)).collect();
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
    /// Take this board into the shared memory budget. False if it does not fit.
    pub fn admit(&self) -> bool {
        let mut st = self.st.lock().unwrap();
        if st.charged > 0 {
            return true;
        }
        let w = weight(st.bytes);
        if !self.mem.admit(w) {
            return false;
        }
        st.charged = w;
        true
    }

    /// Set the bytes held to `bytes`, charging or crediting the shared
    /// budget. False, and nothing changed, if growth does not fit.
    fn resize(&self, st: &mut State, bytes: usize) -> bool {
        if st.charged > 0 {
            let w = weight(bytes);
            if w > st.charged {
                if !self.mem.grow(w - st.charged) {
                    return false;
                }
            } else {
                self.mem.shrink(st.charged - w);
            }
            st.charged = w;
        }
        st.bytes = bytes;
        true
    }

    pub fn new(board: i64, loaded: Loaded, mem: Arc<Memory>) -> Room {
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
                            shelf.push(Sheet { k: r.sheet, strokes: Vec::new(), view: None });
                            shelf.len() - 1
                        }
                    };
                    &mut shelf[i].strokes
                };
                into.push(Stroke {
                    seq: r.seq,
                    layer: r.layer,
                    state: r.state,
                    json: stroke_json(&r.id, r.layer, r.seq, &r.color, &w, &r.pts, r.txt.as_deref(), r.sh.as_deref()),
                    id: r.id.clone(),
                    node: r.sh.as_deref().and_then(box_node).map(str::to_string),
                });
            }
            ids.insert(r.id);
        }
        shelf.sort_by_key(|s| s.k);
        let bytes = strokes.iter().chain(shelf.iter().flat_map(|s| s.strokes.iter())).map(|s| s.json.len()).sum();
        let layers = loaded
            .layers
            .into_iter()
            .map(|(n, who, color)| Layer { n, who, color, name: None, on: 0, pinged: None })
            .collect();
        let (tx, _) = broadcast::channel(256);
        let end = tx.clone();
        let link = Link::new(board, move || {
            let _ = end.send(Out { skip: 0, only: 0, text: Utf8Bytes::from_static(""), end: true });
        });
        Room {
            board,
            tx,
            link,
            mem,
            spare: Arc::new(AtomicBool::new(false)),
            st: Mutex::new(State {
                next_seq,
                strokes,
                shelf,
                next_sheet,
                bytes,
                charged: 0,
                snap: None,
                ids,
                layers,
                live: HashMap::new(),
                conns: 0,
                viewers: 0,
                sheets: Bucket::new(SHEETS_BURST, SHEETS_PER_SEC),
                next_ping: 0,
            }),
        }
    }

    fn all(&self, text: String) {
        let _ = self.tx.send(Out { skip: 0, only: 0, text: text.into(), end: false });
    }
    fn others(&self, conn: u64, text: String) {
        let _ = self.tx.send(Out { skip: conn, only: 0, text: text.into(), end: false });
    }
    fn only(&self, conn: u64, text: String) {
        let _ = self.tx.send(Out { skip: 0, only: conn, text: text.into(), end: false });
    }

    /// A write failed: what this room holds is not what is stored. It takes
    /// no one new, and is read again from storage on the next join.
    pub fn is_dead(&self) -> bool {
        self.link.is_dead()
    }

    pub fn conns(&self) -> usize {
        self.st.lock().unwrap().conns
    }

    /// [`Room::join_from`] for no address in particular (tests).
    pub fn join(
        &self,
        conn: u64,
        role: Role,
        who: Option<[u8; 32]>,
        view: Option<&str>,
        db: &Db,
        may_add: impl FnOnce() -> bool,
    ) -> Result<Joined, NoJoin> {
        self.join_from(conn, role, who, view, db, None, may_add)
    }

    /// Add a connection. `who` is the hash of the contributor secret (edit
    /// links only). `view` is the view key to hand to an editor. `held` is what
    /// the joining address holds of the memory budget (P3-B).
    /// `may_add` is asked, once, before a new contributor is given a layer, and
    /// only once the join's copies are charged (P3-B2): false (the address made
    /// too many lately) makes them a viewer.
    /// Err: the board has no room for another connection, or the budget for its copy.
    #[allow(clippy::too_many_arguments)]
    pub fn join_from(
        &self,
        conn: u64,
        mut role: Role,
        who: Option<[u8; 32]>,
        view: Option<&str>,
        db: &Db,
        held: Option<&Held>,
        may_add: impl FnOnce() -> bool,
    ) -> Result<Joined, NoJoin> {
        let mut st = self.st.lock().unwrap();
        if st.conns >= MAX_ROOM_CONNS {
            return Err(NoJoin::Full);
        }
        // Settle the role before counting it: whoever will be a viewer
        // (including an edit-key holder when the layers are full) takes a
        // viewer's seat.
        enum Seat {
            Mine(usize),
            New,
            Reclaim(usize),
            Full,
        }
        let mut seat = match (role, who) {
            (Role::Edit, None) => return Err(NoJoin::Full),
            (Role::Edit, Some(who)) => match st.layers.iter().position(|l| l.who == who) {
                Some(i) => Seat::Mine(i),
                None if st.layers.len() < MAX_LAYERS => Seat::New,
                None => reclaimable(&st).map_or(Seat::Full, Seat::Reclaim),
            },
            (Role::View, _) => Seat::Full,
        };
        if matches!(seat, Seat::Full) && st.viewers >= MAX_ROOM_VIEWERS {
            return Err(NoJoin::Full);
        }
        // Before anything changes: a build of the snapshot that must be charged may not fit.
        let snap = match &st.snap {
            Some(s) => s.clone(),
            None => {
                let drawn: Vec<&Stroke> = st.strokes.iter().filter(|s| s.state == LIVE).collect();
                let chunks: Vec<Utf8Bytes> = drawn
                    .chunks(SNAPSHOT_CHUNK)
                    .map(|c| strokes_msg(r#"{"t":"ss","a":["#, c.iter().map(|s| s.json.as_str())).into())
                    .collect();
                let bytes: usize = chunks.iter().map(|c| c.len()).sum();
                let counted = self.spare.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst).is_ok();
                if !counted && !self.mem.grow(bytes) {
                    return Err(NoJoin::Busy);
                }
                let extra = if counted { 0 } else { bytes };
                let s = Arc::new(Snap { chunks, counted, extra, spare: self.spare.clone(), mem: self.mem.clone() });
                st.snap = Some(s.clone());
                s
            }
        };
        // Each join's copy of the live strokes is its own: charge it too, before anything changes.
        let mut live = Vec::new();
        for (id, s) in &st.live {
            let w = width_json(s.w);
            live.push(Utf8Bytes::from(format!(r#"{{"t":"b","id":"{id}","l":{},"c":"{}","w":{w}}}"#, s.layer, s.c)));
            if !s.pts.is_empty() {
                live.push(format!(r#"{{"t":"p","id":"{id}","pts":{}}}"#, pts_json(&s.pts)).into());
            }
        }
        let bytes: usize = live.iter().map(|t| t.len()).sum();
        // The address holds this copy and any extra build of the snapshot it keeps alive.
        let addr_bytes = bytes + snap.extra;
        if held.is_some_and(|h| !h.take(addr_bytes, self.mem.addr_cap())) {
            return Err(NoJoin::Busy);
        }
        let addr = held.map(|h| (h.clone(), addr_bytes));
        if !self.mem.grow(bytes) {
            if let Some((h, n)) = &addr {
                h.give(*n);
            }
            return Err(NoJoin::Busy);
        }
        let charge = Charge { bytes, mem: self.mem.clone(), addr };
        // Only now, with the copies charged, may a new contributor spend a layer token (P3-B2).
        if matches!(seat, Seat::New | Seat::Reclaim(_)) && !may_add() {
            seat = Seat::Full;
            if st.viewers >= MAX_ROOM_VIEWERS {
                return Err(NoJoin::Full);
            }
        }
        let full = role == Role::Edit && matches!(seat, Seat::Full);
        let who = who.unwrap_or_default();
        let mut layer = 0;
        match seat {
            Seat::Mine(i) => {
                layer = st.layers[i].n;
                st.layers[i].on += 1;
            }
            Seat::New => {
                layer = st.layers.len() as u32 + 1;
                let color = layer_color(layer);
                db.add_layer(&self.link, layer, who, color.clone());
                self.all(format!(r#"{{"t":"l","l":{layer},"c":"{color}"}}"#));
                st.layers.push(Layer { n: layer, who, color, name: None, on: 1, pinged: None });
            }
            Seat::Reclaim(i) => {
                // An empty layer no one is on: hand it to the newcomer, with
                // its first colour and no name.
                layer = st.layers[i].n;
                let color = layer_color(layer);
                db.add_layer(&self.link, layer, who, color.clone());
                self.all(format!(r#"{{"t":"nm","l":{layer},"n":null,"c":"{color}"}}"#));
                st.layers[i] = Layer { n: layer, who, color, name: None, on: 1, pinged: None };
            }
            Seat::Full => role = Role::View,
        }
        st.conns += 1;
        if role == Role::View {
            st.viewers += 1;
        }
        // Subscribe after the layer notice (the newcomer reads its layer from
        // hello) and before anything else can be broadcast.
        let rx = self.tx.subscribe();
        self.all(count_msg(&st));

        let mut snapshot = Vec::new();
        let mut hello = String::from(r#"{"t":"hello","role":"#);
        hello.push_str(if role == Role::Edit { r#""edit","me":"# } else { r#""view","me":"# });
        match role {
            Role::Edit => hello.push_str(&layer.to_string()),
            Role::View => hello.push_str("null"),
        }
        // Every edit-key holder learns the view key, even one turned into a
        // viewer because the layers are full: the page shows the current key
        // as the view link only when no view key came (a view-key holder).
        match view {
            Some(v) => hello.push_str(&format!(r#","view":"{v}","layers":["#)),
            None => hello.push_str(r#","view":null,"layers":["#),
        }
        hello.push_str(&layers_json(&st));
        hello.push_str(r#"],"shelf":"#);
        hello.push_str(&shelf_json(&st));
        hello.push('}');
        snapshot.push(hello.into());
        if full {
            snapshot.push(Utf8Bytes::from_static(
                r#"{"t":"err","code":"full","m":"This board has no room for another contributor right now, so it opened view-only."}"#,
            ));
        }

        snapshot.extend(snap.chunks.iter().cloned());
        snapshot.extend(live);
        snapshot.push(Utf8Bytes::from_static(r#"{"t":"ready"}"#));
        Ok(Joined { peer: Peer { conn, role, layer }, rx, snapshot, hold: Some(snap), live: Some(charge) })
    }

    /// Remove a connection and abandon the strokes it left unfinished.
    pub fn leave(&self, peer: Peer) {
        let conn = peer.conn;
        let mut st = self.st.lock().unwrap();
        st.conns = st.conns.saturating_sub(1);
        if peer.role == Role::View {
            st.viewers = st.viewers.saturating_sub(1);
        }
        if peer.role == Role::Edit {
            if let Some(l) = st.layers.iter_mut().find(|l| l.n == peer.layer) {
                l.on = l.on.saturating_sub(1);
            }
        }
        let gone: Vec<String> =
            st.live.iter().filter(|(_, s)| s.conn == conn).map(|(id, _)| id.clone()).collect();
        for id in gone {
            st.live.remove(&id);
            self.others(conn, format!(r#"{{"t":"u","id":"{id}"}}"#));
        }
        self.all(count_msg(&st));
    }

    /// Apply one message from a connection that has joined.
    /// `heavy` is the connection's budget for the operations that send a whole
    /// sheet or layer (crumple, bring back, preview, layer restore).
    pub fn handle(&self, peer: Peer, text: &str, db: &Db, budget: &mut Budget) {
        let heavy = &mut budget.heavy;
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
            In::Name { n, c } => {
                let Some(name) = clean_name(&n) else {
                    return;
                };
                if !valid_color(&c) || !heavy.take() {
                    return;
                }
                let color = c.to_ascii_lowercase();
                let Some(l) = st.layers.iter_mut().find(|l| l.n == peer.layer) else {
                    return;
                };
                l.name = Some(name.clone());
                l.color = color.clone();
                let name = serde_json::to_string(&name).unwrap_or_else(|_| "null".into());
                self.all(format!(r#"{{"t":"nm","l":{},"n":{name},"c":"{color}"}}"#, peer.layer));
            }
            In::Ping { pts } => {
                if pts.len() > MAX_PING_POINTS * 3 {
                    return;
                }
                let Some(pts) = clean_pts(pts) else {
                    return;
                };
                if pts.len() < 6 {
                    return;
                }
                let now = Instant::now();
                let Some(l) = st.layers.iter_mut().find(|l| l.n == peer.layer) else {
                    return;
                };
                if l.pinged.is_some_and(|t| now.duration_since(t) < PING_EVERY) {
                    return; // one ping every two seconds per contributor
                }
                l.pinged = Some(now);
                let k = (b'A' + st.next_ping) as char;
                st.next_ping = (st.next_ping + 1) % 26;
                self.all(format!(r#"{{"t":"pg","l":{},"k":"{k}","pts":{}}}"#, peer.layer, pts_json(&pts)));
            }
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
                    // By layer, not connection: after a reconnect the same
                    // browser finishes what its old socket began.
                    Some(s) if s.layer == peer.layer => st.live.remove(&id),
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
                self.commit(&mut st, peer, id, c, w, pts, None, None, was_live, db, &budget.store);
            }
            In::Stroke { id, c, w, pts, txt, sh } => {
                if !valid_id(&id) {
                    return;
                }
                let was_live = match st.live.get(&id) {
                    Some(s) if s.layer == peer.layer => {
                        st.live.remove(&id);
                        true
                    }
                    Some(_) => return,
                    None => false,
                };
                match clean_item(c, w, pts, txt, sh) {
                    Some((c, w, pts, txt, sh)) => {
                        self.commit(&mut st, peer, id, c, w, pts, txt, sh, was_live, db, &budget.store);
                    }
                    None => {
                        if was_live {
                            self.others(conn, format!(r#"{{"t":"u","id":"{id}"}}"#));
                        }
                        self.only(conn, refusal(&id, "the stroke was not valid"));
                    }
                }
            }
            In::Batch { a } => self.batch(&mut st, peer, a, db, &budget.store),
            In::Undo { id } => {
                if !valid_id(&id) {
                    return;
                }
                if st.live.get(&id).is_some_and(|s| s.layer == peer.layer) {
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
                let gone = st.strokes.remove(i);
                let left = st.bytes.saturating_sub(gone.json.len());
                self.resize(&mut st, left);
                st.snap = None;
                db.stroke_state(&self.link, id.clone(), UNDONE);
                self.others(conn, format!(r#"{{"t":"u","id":"{id}"}}"#));
            }
            In::LayerDelete { l } => {
                let mut any = false;
                for s in st.strokes.iter_mut().filter(|s| s.layer == l && s.state == LIVE) {
                    s.state = DELETED;
                    any = true;
                }
                if any {
                    st.snap = None;
                    db.layer_state(&self.link, l, LIVE, DELETED);
                    self.all(format!(r#"{{"t":"ld","l":{l}}}"#));
                }
            }
            In::Crumple {} => {
                if st.strokes.is_empty() || !heavy.take() {
                    return;
                }
                st.snap = None;
                let k = st.next_sheet;
                st.next_sheet += 1;
                let sheet = std::mem::take(&mut st.strokes);
                db.sheet_move(&self.link, 0, k);
                st.shelf.push(Sheet { k, strokes: sheet, view: None });
                let mut dropped = String::from("null");
                if st.shelf.len() > MAX_SHELF {
                    let old = st.shelf.remove(0);
                    for s in &old.strokes {
                        st.ids.remove(&s.id);
                        let left = st.bytes.saturating_sub(s.json.len());
                        self.resize(&mut st, left);
                    }
                    db.sheet_drop(&self.link, old.k);
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
                let size = sheet_size(st.shelf[i].strokes.iter().filter(|s| s.state == LIVE));
                if !heavy.take() || !may_broadcast(&mut st.sheets, budget, size) {
                    return;
                }
                st.snap = None;
                let back = st.shelf.remove(i);
                let cur = std::mem::replace(&mut st.strokes, back.strokes);
                // Storage: first move the sheet in use out of the way, then bring k in.
                let nk = st.next_sheet;
                st.next_sheet += 1;
                if cur.is_empty() {
                    // Only undone rows can be left on sheet 0; they go for good.
                    db.sheet_drop(&self.link, 0);
                } else {
                    db.sheet_move(&self.link, 0, nk);
                    st.shelf.push(Sheet { k: nk, strokes: cur, view: None });
                }
                db.sheet_move(&self.link, k, 0);
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
                let Some(sheet) = st.shelf.iter_mut().find(|s| s.k == k) else { return };
                // A cached preview costs as much to send as a new one.
                if !heavy.take() {
                    return;
                }
                if sheet.view.is_none() {
                    let head = format!(r#"{{"t":"sv","k":{k},"a":["#);
                    let drawn = sheet.strokes.iter().filter(|s| s.state == LIVE).map(|s| s.json.as_str());
                    sheet.view = Some(strokes_msg(&head, drawn).into());
                }
                let text = sheet.view.clone().expect("just built");
                if !budget.send(text.len()) {
                    return;
                }
                let _ = self.tx.send(Out { skip: 0, only: conn, text, end: false });
            }
            In::LayerRestore { l } => {
                if !st.strokes.iter().any(|s| s.layer == l && s.state == DELETED) {
                    return;
                }
                let size = sheet_size(st.strokes.iter().filter(|s| s.layer == l && s.state == DELETED));
                if !heavy.take() || !may_broadcast(&mut st.sheets, budget, size) {
                    return;
                }
                st.snap = None;
                let mut back: Vec<&str> = Vec::new();
                for s in st.strokes.iter_mut().filter(|s| s.layer == l && s.state == DELETED) {
                    s.state = LIVE;
                    back.push(&s.json);
                }
                if !back.is_empty() {
                    db.layer_state(&self.link, l, DELETED, LIVE);
                    let head = format!(r#"{{"t":"lr","l":{l},"a":["#);
                    self.all(strokes_msg(&head, back.into_iter()));
                }
            }
        }
    }

    /// The author's `ok`, to send from the storage thread once the stroke is
    /// committed. An ok is a promise that the stroke is on disk: the page
    /// forgets its own copy on it.
    fn ok_once_stored(&self, conn: u64, id: &str, seq: u64) -> crate::db::Ack {
        let tx = self.tx.clone();
        let text = format!(r#"{{"t":"ok","id":"{id}","q":{seq}}}"#);
        Box::new(move || {
            let _ = tx.send(Out { skip: 0, only: conn, text: text.into(), end: false });
        })
    }

    /// `x`: several `s` and `u` of my own layer as one change. Every part is checked first;
    /// if any is refused, none is applied. The sender then gets `no` for each new item and its
    /// own copies of the items it meant to take away, so its board matches the relay's again.
    /// Applied, the parts go to the others as one `x` and to storage in one transaction.
    fn batch(&self, st: &mut State, peer: Peer, a: Vec<serde_json::Value>, db: &Db, store: &Mutex<Bucket>) {
        let conn = peer.conn;
        let mut gone: Vec<String> = Vec::new();
        let mut new: Vec<(String, String, f32, Vec<f32>, Option<String>, Option<String>)> = Vec::new();
        let mut why = None;
        let mut new_ids = HashSet::new();
        if a.is_empty() || a.len() > MAX_BATCH {
            why = Some("the change was too big");
        }
        for v in a.into_iter().take(MAX_BATCH) {
            match serde_json::from_value::<In>(v) {
                Ok(In::Undo { id }) if valid_id(&id) && !gone.contains(&id) => {
                    let mine = st.strokes.iter().any(|s| s.id == id && s.layer == peer.layer && s.state == LIVE);
                    if !mine {
                        why = why.or(Some("part of the change was gone"));
                    }
                    gone.push(id);
                }
                Ok(In::Stroke { id, c, w, pts, txt, sh }) if valid_id(&id) => {
                    if st.ids.contains(&id) || st.live.contains_key(&id) || !new_ids.insert(id.clone()) {
                        why = why.or(Some("the change was not valid"));
                        continue;
                    }
                    match clean_item(c, w, pts, txt, sh) {
                        Some((c, w, pts, txt, sh)) => new.push((id, c, w, pts, txt, sh)),
                        None => {
                            why = why.or(Some("the change was not valid"));
                            new.push((id, String::new(), 0.0, Vec::new(), None, None));
                        }
                    }
                }
                _ => why = why.or(Some("the change was not valid")),
            }
        }
        if new.iter().any(|n| n.5.as_deref().and_then(box_node).is_some_and(|n| node_taken(st, n, peer.layer))) {
            why = why.or(Some("another contributor's shape already has that name"));
        }
        let sizes: Vec<(String, String)> = new.iter().map(|n| (width_json(n.2), pts_json(&n.3))).collect();
        let size: usize = new
            .iter()
            .zip(&sizes)
            .map(|(n, (_, pj))| pj.len() + n.4.as_ref().map_or(0, |t| t.len() * 2) + n.5.as_ref().map_or(0, String::len) + 96)
            .sum();
        if why.is_none() {
            if db.is_full() {
                why = Some("storage is full; try again later");
            } else if st.strokes.len() + st.shelf.iter().map(|s| s.strokes.len()).sum::<usize>() + new.len() > MAX_STROKES
                || st.bytes + size > MAX_BOARD_BYTES
                || st.ids.len() + new.len() > MAX_IDS
            {
                why = Some("this board is full");
            } else if !store.lock().unwrap().take_n(size as f64) {
                why = Some("too much has been stored from your network lately; try again later");
            }
        }
        // Memory: what the board holds once the parts are applied.
        let freed: usize = st.strokes.iter().filter(|s| gone.contains(&s.id) && s.layer == peer.layer && s.state == LIVE).map(|s| s.json.len()).sum();
        let seq0 = st.next_seq;
        let jsons: Vec<String> = new
            .iter()
            .zip(&sizes)
            .enumerate()
            .map(|(i, (n, (wj, pj)))| stroke_json(&n.0, peer.layer, seq0 + i as u64, &n.1, wj, pj, n.4.as_deref(), n.5.as_deref()))
            .collect();
        if why.is_none() {
            let more = (st.bytes + jsons.iter().map(String::len).sum::<usize>()).saturating_sub(freed);
            if !self.resize(st, more) {
                store.lock().unwrap().give(size as f64);
                why = Some("the server is out of memory for strokes; try again later");
            }
        }
        if let Some(why) = why {
            for n in &new {
                self.only(conn, refusal(&n.0, why));
            }
            for id in &gone {
                if let Some(s) = st.strokes.iter().find(|s| &s.id == id && s.layer == peer.layer && s.state == LIVE) {
                    self.only(conn, format!(r#"{{"t":"e",{}"#, &s.json[1..]));
                }
            }
            return;
        }
        // Apply: the old items go, then the new ones come, in the order sent.
        let mut out: Vec<String> = Vec::new();
        let mut cmds = Vec::new();
        for id in &gone {
            if let Some(i) = st.strokes.iter().rposition(|s| &s.id == id && s.layer == peer.layer && s.state == LIVE) {
                st.strokes.remove(i);
            }
            cmds.push(db.stroke_state_cmd(&self.link, id.clone(), UNDONE));
            out.push(format!(r#"{{"t":"u","id":"{id}"}}"#));
        }
        let mut oks = Vec::new();
        for ((n, (_, pj)), json) in new.into_iter().zip(sizes).zip(jsons) {
            let seq = st.next_seq;
            st.next_seq += 1;
            let (id, c, w, _, txt, sh) = n;
            out.push(format!(r#"{{"t":"e",{}"#, &json[1..]));
            oks.push(self.ok_once_stored(conn, &id, seq));
            cmds.push(db.add_stroke_cmd(&self.link, seq, id.clone(), peer.layer, c, f64::from(w), pj, txt, sh.clone()));
            st.ids.insert(id.clone());
            let node = sh.as_deref().and_then(box_node).map(str::to_string);
            st.strokes.push(Stroke { seq, layer: peer.layer, state: LIVE, id, json, node });
        }
        st.snap = None;
        self.others(conn, format!(r#"{{"t":"x","a":[{}]}}"#, out.join(",")));
        db.many(&self.link, cmds, Box::new(move || oks.into_iter().for_each(|ok| ok())));
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
        sh: Option<String>,
        was_live: bool,
        db: &Db,
        store: &Mutex<Bucket>,
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
            // A resend. Confirm it again only if it is still this layer's stroke on the sheet
            // in use. Crumpled onto the shelf, deleted with its layer or undone, it is not on
            // the author's sheet any more: `u` takes their copy away instead of leaving a ghost.
            drop_live(self);
            match st.strokes.iter().rev().find(|s| s.id == id) {
                Some(s) if s.layer == peer.layer && s.state == LIVE => {
                    // Not before the first copy is committed.
                    let ok = self.ok_once_stored(conn, &id, s.seq);
                    db.ack(&self.link, ok);
                }
                _ => self.only(conn, format!(r#"{{"t":"u","id":"{id}"}}"#)),
            }
            return;
        }
        if sh.as_deref().and_then(box_node).is_some_and(|n| node_taken(st, n, peer.layer)) {
            drop_live(self);
            self.only(conn, refusal(&id, "another contributor's shape already has that name"));
            return;
        }
        if db.is_full() {
            // Refused before it is sent on: a stroke the others see must be stored.
            drop_live(self);
            self.only(conn, refusal(&id, "storage is full; try again later"));
            return;
        }
        let (wj, pj) = (width_json(w), pts_json(&pts));
        let size = pj.len() + txt.as_ref().map_or(0, |t| t.len() * 2) + sh.as_ref().map_or(0, String::len) + 96;
        if st.strokes.len() + st.shelf.iter().map(|s| s.strokes.len()).sum::<usize>() >= MAX_STROKES
            || st.bytes + size > MAX_BOARD_BYTES
            || st.ids.len() >= MAX_IDS
        {
            drop_live(self);
            self.only(conn, refusal(&id, "this board is full"));
            return;
        }
        // The address's share of storage (H5), taken before anything is kept or sent on.
        if !store.lock().unwrap().take_n(size as f64) {
            drop_live(self);
            self.only(conn, refusal(&id, "too much has been stored from your network lately; try again later"));
            return;
        }
        let seq = st.next_seq;
        st.next_seq += 1;
        let json = stroke_json(&id, peer.layer, seq, &c, &wj, &pj, txt.as_deref(), sh.as_deref());
        let more = st.bytes + json.len();
        if !self.resize(st, more) {
            store.lock().unwrap().give(size as f64);
            drop_live(self);
            self.only(conn, refusal(&id, "the server is out of memory for strokes; try again later"));
            return;
        }
        self.others(conn, format!(r#"{{"t":"e",{}"#, &json[1..]));
        let ok = self.ok_once_stored(conn, &id, seq);
        let node = sh.as_deref().and_then(box_node).map(str::to_string);
        db.add_stroke(&self.link, seq, id.clone(), peer.layer, c, f64::from(w), pj, txt, sh, ok);
        st.ids.insert(id.clone());
        st.snap = None;
        st.strokes.push(Stroke { seq, layer: peer.layer, state: LIVE, id, json, node });
    }
}

/// The head count, with the layers that have a connection open: `{t:"n", n, on:[l..]}`.
/// A layer that can be handed to a new contributor: no one is on it and it
/// holds nothing, on the sheet in use, on the shelf or being drawn.
fn reclaimable(st: &State) -> Option<usize> {
    if !st.layers.iter().any(|l| l.on == 0) {
        return None;
    }
    let used: HashSet<u32> = st
        .strokes
        .iter()
        .chain(st.shelf.iter().flat_map(|s| s.strokes.iter()))
        .map(|s| s.layer)
        .chain(st.live.values().map(|s| s.layer))
        .collect();
    st.layers.iter().position(|l| l.on == 0 && !used.contains(&l.n))
}

fn count_msg(st: &State) -> String {
    let on: Vec<String> = st.layers.iter().filter(|l| l.on > 0).map(|l| l.n.to_string()).collect();
    format!(r#"{{"t":"n","n":{},"on":[{}]}}"#, st.conns, on.join(","))
}

/// A display name: 1 to 24 characters, single spaces, nothing unprintable.
fn clean_name(n: &str) -> Option<String> {
    let n: String = n.chars().filter(|&c| !invisible(c)).collect();
    let n = n.split_whitespace().collect::<Vec<_>>().join(" ");
    let len = n.chars().count();
    if len == 0 || len > 24 || n.chars().any(char::is_control) {
        return None;
    }
    Some(n)
}

/// The layers as they go in hello and un: `{l, c, del, n}` objects, comma separated.
fn layers_json(st: &State) -> String {
    let mut out = String::new();
    for (i, l) in st.layers.iter().enumerate() {
        let del = st.strokes.iter().any(|s| s.layer == l.n && s.state == DELETED);
        if i > 0 {
            out.push(',');
        }
        let name = l.name.as_ref().and_then(|n| serde_json::to_string(n).ok()).unwrap_or_else(|| "null".into());
        out.push_str(&format!(r#"{{"l":{},"c":"{}","del":{del},"n":{name}}}"#, l.n, l.color));
    }
    out
}

/// About the bytes of a frame holding `strokes`.
fn sheet_size<'a>(strokes: impl Iterator<Item = &'a Stroke>) -> usize {
    256 + strokes.map(|s| s.json.len() + 1).sum::<usize>()
}

/// Whether a whole-sheet frame of `size` bytes may go to everyone: it is
/// charged to the board, so many addresses cannot add up, and to the asking
/// address, so its many connections cannot either. One copy is charged,
/// however many receive it.
fn may_broadcast(sheets: &mut Bucket, budget: &mut Budget, size: usize) -> bool {
    if !sheets.take_n(size as f64) {
        return false;
    }
    if !budget.send(size) {
        sheets.give(size as f64);
        return false;
    }
    true
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
        assert_eq!(pts_json(&[-1.0, -0.5625, 0.5, -0.000004, -0.25, 1.0]), "[-1,-0.5625,0.5,0,-0.25,1]");
        assert_eq!(width_json(0.004), "0.004");
        assert_eq!(width_json(0.0000001), "0.00001");
    }

    #[test]
    fn points_are_clamped_and_checked() {
        assert_eq!(clean_pts(vec![-1.5, 2.0, 5.0]).unwrap(), vec![-1.0, 2.0 * ASPECT, 1.0]);
        assert_eq!(clean_pts(vec![2.5, -3.0, -1.0]).unwrap(), vec![2.0, -ASPECT, 0.0]);
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
        // Bidi controls and invisible characters go; joiners stay.
        assert_eq!(clean_text("abc\u{202E}fed\u{2066}x\u{2069}\u{200B}\u{FEFF}").unwrap(), "abcfedx");
        assert!(clean_text("\u{200B}\u{202E}").is_none(), "nothing visible is left");
        assert_eq!(clean_text("👩\u{200D}🏫 می\u{200C}خواهم").unwrap(), "👩\u{200D}🏫 می\u{200C}خواهم");
    }

    #[test]
    fn names_are_checked() {
        assert_eq!(clean_name("  Amber   Otter ").unwrap(), "Amber Otter");
        assert_eq!(clean_name("Tea\u{202E}rehc").unwrap(), "Tearehc", "an override cannot make it read as Teacher");
        assert_eq!(clean_name("Ann\u{2067}\u{200F}e").unwrap(), "Anne");
        assert!(clean_name("\u{200B}\u{2060}").is_none());
        assert!(clean_name("tab\u{7}").is_none());
        assert!(clean_name(&"x".repeat(25)).is_none());
        assert!(clean_name(&format!("{}\u{200B}", "x".repeat(24))).is_some(), "invisibles do not count");
        assert_eq!(clean_name("👨\u{200D}👩\u{200D}👧").unwrap(), "👨\u{200D}👩\u{200D}👧");
        let j = stroke_json("a", 1, 2, "#111111", "0.02", "[0.1,0.2,0.5]", Some("say \"hi\"\n</script>"), None);
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
