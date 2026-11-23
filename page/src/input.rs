//! Pointer input and text annotations: app.js's pointer handlers, the wheel and Safari gesture
//! zoom, the text box and undo.
//!
//! Pen: draws. Mouse: left button draws; middle button or Space+drag pans. Fingers: two pinch
//! and pan; one draws, until a pen has been seen, after which single fingers are ignored (a
//! resting hand). On a view link one finger or the mouse pans.
//!
//! [`Input`] holds only the gesture state. Everything else (the board, the view, the socket,
//! the canvases, the text box element) is behind [`Host`], called in the order app.js did, so
//! each handler's branching is tested with a recording host and no browser.

use crate::geom::{self, r5, ERASER_COLOR, ERASER_SCALE, EPS, LINE_HEIGHT, TEXT_SCALE};
use crate::model::Board;
use crate::geom::Op;
use crate::proto::{Item, Layer, Out};
use crate::shape::{self, Anchor, BoxKind, Heads, Hover, Node, Scene, Shape, Style};
use crate::view::{clamp, js_round, View, MAX_ZOOM, WH, WY};
use indexmap::IndexMap;
use std::collections::HashMap;

/// A stroke holds at most 20,000 points.
pub const MAX_PTS: usize = 60_000;
/// A text annotation keeps at most this many lines.
pub const MAX_LINES: usize = 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Mouse,
    Pen,
    Touch,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool {
    Pen,
    Erase,
    Text,
    Move,
    Ping,
    /// Drag out a shape; drag one of mine to move it.
    Shape(BoxKind),
    /// Drag a straight line; an end dropped on a shape attaches to it.
    Line,
    /// Pick my items (click, shift-click, a marquee) to move, delete or group them.
    Select,
    /// Fill one of my shapes with the current colour; the same colour again clears it.
    Bucket,
}

/// One pointer event, or one of its coalesced samples. `x`, `y` are client px.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Ptr {
    pub id: i32,
    pub kind: Kind,
    pub button: i16,
    pub buttons: u16,
    pub pressure: f64,
    pub x: f64,
    pub y: f64,
    pub primary: bool,
    /// Shift held: a click adds to the selection.
    pub shift: bool,
}

/// What the toolbar and `hello` have set.
#[derive(Clone, Debug, PartialEq)]
pub struct Pen {
    /// `role === 'edit'`.
    pub edit: bool,
    pub me: Option<Layer>,
    pub tool: Tool,
    pub color: Option<String>,
    pub my_color: Option<String>,
    pub width: f64,
    /// New shapes are filled, not outlined.
    pub fill: bool,
    /// Which ends of a new line carry an arrowhead.
    pub heads: Heads,
    /// The line style, fill colour and bold/italic new items get (never groups).
    pub style: Style,
    /// Snap to grid is on (per viewer, never synced).
    pub snap: bool,
}

/// The socket as input sees it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Net {
    /// `hello` received on the current socket.
    pub online: bool,
    /// The snapshot is complete on the current socket.
    pub ready: bool,
    /// A private board: only in this browser.
    pub local: bool,
}

/// The rest of the page. Effects are named after the app.js functions they stand for.
pub trait Host {
    fn board(&mut self) -> &mut Board;
    fn view(&self) -> &View;
    /// Ids of my finished items, for undo.
    fn mine(&mut self) -> &mut Vec<String>;
    fn pen(&self) -> Pen;
    fn net(&self) -> Net;
    fn hidden(&self, l: Option<Layer>) -> bool;
    /// `performance.now()`.
    fn now(&self) -> f64;
    /// A new item id.
    fn new_id(&mut self) -> String;
    /// The board element's left and top, client px.
    fn rect(&self) -> (f64, f64);
    /// The item's bounding box (`bbox`, cached).
    fn bbox(&mut self, s: &Item) -> [f64; 4];

    fn send(&mut self, o: Out);
    fn set_view(&mut self, z: f64, x: f64, y: f64, settle: bool);
    fn zoom_at(&mut self, f: f64, sx: f64, sy: f64, settle: bool);
    fn settle_view(&mut self);
    fn capture(&mut self, pointer: i32);
    /// The `drawing` class on the body.
    fn set_drawing(&mut self, on: bool);
    /// Ask for an animation frame that calls [`Input::flush`].
    fn request_flush(&mut self);
    fn draw_new(&mut self, id: &str);
    fn redraw_live(&mut self);
    fn insert_stroke(&mut self, s: Item);
    fn remove_stroke(&mut self, id: &str) -> bool;
    fn send_ping(&mut self, pts: Vec<f64>, touch: bool);
    /// A ping ended: back to the pen (`tool = 'pen'; markTools()`).
    fn ping_done(&mut self);
    fn save_local(&mut self);
    /// Show the text box with `text`, place it ([`editor_box`]) and focus it.
    fn editor_show(&mut self, ed: &Editor, text: &str);
    /// The text box's value.
    fn editor_text(&self) -> String;
    /// Hide, empty and blur the text box.
    fn editor_hide(&mut self);
    /// Where every visible shape and line is (rebuilt when the board changes).
    fn scene(&mut self) -> &Scene;
    /// Show `o` over the live canvas until the next call; `None` clears it.
    fn overlay(&mut self, o: Option<Overlay>);
    /// The selection changed: whether Group and Ungroup would do anything now.
    fn sel_changed(&mut self, can_group: bool, can_ungroup: bool);
}

/// What the page draws over the live canvas during a shape gesture: ghosts of the result, and
/// the anchors a line end can attach to.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Overlay {
    /// Each ghost's colour and ops (board px), drawn at half opacity.
    pub ghosts: Vec<(String, Vec<Op>)>,
    /// Anchor dots, board units; `true` for the one a release would attach to.
    pub dots: Vec<(f64, f64, bool)>,
    /// The selection's box, board units.
    pub sel: Option<[f64; 4]>,
    /// The marquee being dragged, board units.
    pub marquee: Option<[f64; 4]>,
}

/// One end of a line being drawn: where it is, and the anchor it is on.
#[derive(Clone, Debug, PartialEq)]
pub struct End {
    pub x: f64,
    pub y: f64,
    pub at: Option<Anchor>,
}

/// A shape gesture in progress (board units).
#[derive(Clone, Debug, PartialEq)]
pub enum Gest {
    NewBox { pointer: i32, kind: BoxKind, x0: f64, y0: f64, x1: f64, y1: f64 },
    MoveBox { pointer: i32, old: Item, sx: f64, sy: f64, dx: f64, dy: f64 },
    Line { pointer: i32, a: End, b: End },
    /// A marquee from empty board; `base` is what was selected before (kept with Shift).
    Marquee { pointer: i32, base: Vec<String>, x0: f64, y0: f64, x1: f64, y1: f64 },
    /// The selection being dragged.
    MoveSel { pointer: i32, sx: f64, sy: f64, dx: f64, dy: f64, ox: f64, oy: f64 },
}

impl Gest {
    fn pointer(&self) -> i32 {
        match self {
            Gest::NewBox { pointer, .. }
            | Gest::MoveBox { pointer, .. }
            | Gest::Line { pointer, .. }
            | Gest::Marquee { pointer, .. }
            | Gest::MoveSel { pointer, .. } => *pointer,
        }
    }
}

/// The stroke being drawn here.
#[derive(Clone, Debug, PartialEq)]
pub struct Cur {
    pub id: String,
    pub pointer: i32,
    /// Points not sent yet.
    pub out: Vec<f64>,
    pub sent: bool,
    pub pen: bool,
    pub ping: bool,
    pub touch: bool,
}

/// A two-finger zoom in progress.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pinch {
    pub d0: f64,
    pub z0: f64,
    pub bx: f64,
    pub by: f64,
}

/// A one-pointer pan in progress.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Drag {
    pub pointer: i32,
    pub x: f64,
    pub y: f64,
}

/// The text box while it is open. `old` is my text being edited, off the board meanwhile.
#[derive(Clone, Debug, PartialEq)]
pub struct Editor {
    pub x: f64,
    pub y: f64,
    pub c: String,
    pub w: f64,
    pub old: Option<Item>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Input {
    pub cur: Option<Cur>,
    /// A pen has been used in this browser.
    pub pen_seen: bool,
    /// Time of the last pen event.
    pub pen_at: f64,
    flush_queued: bool,
    /// Fingers on the board, in the order they landed.
    pub touches: IndexMap<i32, (f64, f64)>,
    pub pinch: Option<Pinch>,
    pub drag: Option<Drag>,
    /// A tap that will place text on release.
    pub text_tap: Option<i32>,
    pub space_down: bool,
    gesture_zoom: f64,
    last_touch_at: f64,
    pub editor: Option<Editor>,
    pub gest: Option<Gest>,
    /// The progressive anchors under a line end.
    pub hover: Hover,
    /// An overlay is showing.
    shown: bool,
    /// Edits that replace or remove items, by the id `mine` holds for them: what undo puts
    /// back. A plain new item has none; undo just takes it away.
    pub steps: HashMap<String, Step>,
    /// My selected items (every member of a selected group).
    pub sel: Vec<String>,
    /// The groups entered by double-click, outermost first: clicks pick inside the last.
    pub group_path: Vec<String>,
    /// The last select click: when, and on which item (for a double-click).
    last_click: Option<(f64, String)>,
}

/// One change of my items, undone as one: `gone` are the items it took away (put back with
/// new ids by undo, so a moved box returns with its name and its arrows), `new` the ids it added.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Step {
    pub gone: Vec<Item>,
    pub new: Vec<String>,
}

impl Default for Input {
    fn default() -> Self {
        Input {
            cur: None,
            pen_seen: false,
            pen_at: -1e9,
            flush_queued: false,
            touches: IndexMap::new(),
            pinch: None,
            drag: None,
            text_tap: None,
            space_down: false,
            gesture_zoom: 1.0,
            last_touch_at: -1e9,
            editor: None,
            gest: None,
            hover: Hover::default(),
            steps: HashMap::new(),
            shown: false,
            sel: vec![],
            group_path: vec![],
            last_click: None,
        }
    }
}

/// `viewer`: a view link, or no layer yet.
fn viewer(p: &Pen) -> bool {
    !p.edit || p.me.is_none()
}

fn online_ready(h: &impl Host) -> bool {
    let n = h.net();
    n.online && n.ready
}

/// The two fingers of a pinch: their midpoint (board-relative px) and their distance.
fn two(touches: &IndexMap<i32, (f64, f64)>, rect: (f64, f64)) -> (f64, f64, f64) {
    let mut it = touches.values();
    let (a, b) = (it.next().copied().unwrap_or_default(), it.next().copied().unwrap_or_default());
    ((a.0 + b.0) / 2.0 - rect.0, (a.1 + b.1) / 2.0 - rect.1, geom::hypot(a.0 - b.0, a.1 - b.1))
}

impl Input {
    /// `addPoint`.
    fn add_point(&mut self, h: &mut impl Host, e: &Ptr, rect: (f64, f64)) {
        let Some(cur) = self.cur.as_mut() else { return };
        let v = h.view();
        let (bx, by) = v.board_point(e.x - rect.0, e.y - rect.1);
        let zoom = v.zoom;
        let Some(s) = h.board().live.get_mut(&cur.id) else { return };
        if e.kind == Kind::Touch {
            cur.touch = true;
        }
        let (x, y) = (r5(bx), r5(by));
        let p = if e.kind == Kind::Pen && e.pressure > 0.0 { js_round(e.pressure * 1000.0) / 1000.0 } else { 0.5 };
        let n = s.pts.len();
        if n > 0 && (s.pts[n - 3] - x).abs() + (s.pts[n - 2] - y).abs() < 0.0003 / zoom.max(1.0) {
            return; // jitter
        }
        if n >= MAX_PTS {
            return;
        }
        s.pts.extend_from_slice(&[x, y, p]);
        if cur.sent {
            cur.out.extend_from_slice(&[x, y, p]);
        }
    }

    /// `queueFlush`: draw and send at most one batch of points per animation frame.
    fn queue_flush(&mut self, h: &mut impl Host) {
        if self.flush_queued {
            return;
        }
        self.flush_queued = true;
        h.request_flush();
    }

    /// The animation frame [`Host::request_flush`] asked for.
    pub fn flush(&mut self, h: &mut impl Host) {
        self.flush_queued = false;
        let Some(cur) = self.cur.as_mut() else { return };
        if h.board().live.contains_key(&cur.id) {
            h.draw_new(&cur.id);
        }
        if cur.sent && !cur.out.is_empty() {
            h.send(Out::Points { id: cur.id.clone(), pts: std::mem::take(&mut cur.out) });
        }
    }

    /// `abandonStroke`: a second finger turned the stroke into a pinch.
    fn abandon_stroke(&mut self, h: &mut impl Host) {
        let Some(cur) = self.cur.take() else { return };
        h.set_drawing(false);
        if h.board().end_live(&cur.id) {
            h.redraw_live();
        }
        if cur.sent {
            h.send(Out::Undo { id: cur.id });
        }
    }

    /// `startPinch`.
    fn start_pinch(&mut self, h: &mut impl Host) {
        if self.cur.as_ref().is_some_and(|c| !c.pen) {
            self.abandon_stroke(h);
        }
        self.drag = None;
        self.text_tap = None;
        let (mx, my, d) = two(&self.touches, h.rect());
        let v = h.view();
        let k = v.bw * v.zoom;
        self.pinch = Some(Pinch { d0: if d == 0.0 { 1.0 } else { d }, z0: v.zoom, bx: v.pan_x + mx / k, by: v.pan_y + my / k });
    }

    /// `pointerdown`. Returns whether to `preventDefault`.
    pub fn down(&mut self, h: &mut impl Host, e: &Ptr) -> bool {
        let t = e.kind;
        let now = h.now();
        if t == Kind::Pen {
            self.pen_seen = true;
            self.pen_at = now;
        }
        if t == Kind::Touch {
            self.last_touch_at = now;
            // A hand landing while the pen is at work is not a gesture.
            if self.cur.as_ref().is_some_and(|c| c.pen) || now - self.pen_at < 400.0 {
                return false;
            }
            // A first finger means no others are down: forget any whose lift was lost.
            if e.primary && !self.touches.is_empty() {
                self.touches.clear();
                self.pinch = None;
            }
            self.touches.insert(e.id, (e.x, e.y));
            if self.touches.len() == 2 {
                self.start_pinch(h);
                return true;
            }
            if self.touches.len() > 2 {
                return false;
            }
        }
        if self.pinch.is_some() {
            return false;
        }
        if self.editor.is_some() {
            // a tap outside the text box finishes it; a resting hand does not
            if !(t == Kind::Touch && self.pen_seen) {
                self.close_editor(h, true);
            }
            return true;
        }
        let pen = h.pen();
        let viewer = viewer(&pen);
        let primary = t != Kind::Mouse || e.button == 0;
        if (t == Kind::Mouse && (e.button == 1 || (e.button == 0 && self.space_down)))
            || (primary && (viewer || pen.tool == Tool::Move))
        {
            h.capture(e.id);
            self.drag = Some(Drag { pointer: e.id, x: e.x, y: e.y });
            return true;
        }
        if t == Kind::Mouse && e.button != 0 {
            return true;
        }
        let Some(color) = pen.color.clone() else { return true };
        if viewer || self.cur.is_some() {
            return true;
        }
        if pen.tool == Tool::Bucket {
            self.bucket(h, e, &color);
            return true;
        }
        if t == Kind::Touch && self.pen_seen {
            return true;
        }
        h.capture(e.id);
        if pen.tool == Tool::Text {
            self.text_tap = Some(e.id);
            return true;
        }
        if matches!(pen.tool, Tool::Shape(_) | Tool::Line) {
            self.shape_down(h, e, &pen);
            return true;
        }
        if pen.tool == Tool::Select {
            self.select_down(h, e);
            return true;
        }
        // Button 5 / buttons bit 32 is the eraser end of a pen.
        let pen_eraser = t == Kind::Pen && (e.button == 5 || e.buttons & 32 != 0);
        let ping = pen.tool == Tool::Ping && !pen_eraser;
        let erase = !ping && (pen.tool == Tool::Erase || pen_eraser);
        let id = h.new_id();
        let c = if ping {
            pen.my_color.clone().unwrap_or_default()
        } else if erase {
            ERASER_COLOR.to_string()
        } else {
            color
        };
        let w = h.view().pen_w(if ping {
            0.004
        } else if erase {
            pen.width * ERASER_SCALE
        } else {
            pen.width
        });
        h.board().begin(&id, pen.me, &c, w);
        let n = h.net();
        // A ping is drawn here only; it goes out as one message when it ends.
        let sent = !ping && n.online && n.ready;
        self.cur = Some(Cur { id: id.clone(), pointer: e.id, out: vec![], sent, pen: t == Kind::Pen, ping, touch: false });
        h.set_drawing(true);
        if sent {
            h.send(Out::Begin { id, c, w });
        }
        let rect = h.rect();
        self.add_point(h, e, rect);
        self.queue_flush(h);
        true
    }

    /// `pointermove`. `coalesced` are the event's coalesced samples (may be empty).
    pub fn moved(&mut self, h: &mut impl Host, e: &Ptr, coalesced: &[Ptr]) {
        if e.kind == Kind::Pen {
            self.pen_at = h.now();
        }
        if e.kind == Kind::Touch {
            if let Some(f) = self.touches.get_mut(&e.id) {
                *f = (e.x, e.y);
                if let Some(p) = self.pinch {
                    if self.touches.len() >= 2 {
                        let (mx, my, d) = two(&self.touches, h.rect());
                        let v = h.view();
                        let z = MAX_ZOOM.min(v.min_zoom().max(p.z0 * d / p.d0));
                        let k = v.bw * z;
                        h.set_view(z, p.bx - mx / k, p.by - my / k, false);
                        return;
                    }
                }
            }
        }
        if let Some(d) = self.drag.as_mut() {
            if e.id == d.pointer {
                let v = h.view();
                let k = v.bw * v.zoom;
                let (z, x, y) = (v.zoom, v.pan_x - (e.x - d.x) / k, v.pan_y - (e.y - d.y) / k);
                h.set_view(z, x, y, false);
                d.x = e.x;
                d.y = e.y;
                return;
            }
        }
        if let Some(g) = self.gest.as_ref().filter(|g| g.pointer() == e.id) {
            if matches!(g, Gest::Marquee { .. } | Gest::MoveSel { .. }) {
                self.select_move(h, e);
            } else {
                self.shape_move(h, e);
            }
            return;
        }
        if self.cur.is_none() && self.gest.is_none() {
            self.line_hover(h, e);
        }
        if self.cur.as_ref().is_none_or(|c| c.pointer != e.id) {
            return;
        }
        let rect = h.rect();
        // Coalesced events give every pen sample, not just one per frame.
        if coalesced.is_empty() {
            self.add_point(h, e, rect);
        } else {
            for c in coalesced {
                self.add_point(h, c, rect);
            }
        }
        self.queue_flush(h);
    }

    /// `endStroke`.
    fn end_stroke(&mut self, h: &mut impl Host, pointer: i32) {
        if self.cur.as_ref().is_none_or(|c| c.pointer != pointer) {
            return;
        }
        let Some(Cur { id, sent, out, touch, ping, .. }) = self.cur.take() else { return };
        let v = h.view();
        let (bw, zoom) = (v.bw, v.zoom);
        // Tolerance: a third of a pixel at 100%, never looser when zoomed out.
        let eps = if touch { 0.3 / (bw * zoom) } else { EPS / zoom.max(1.0) };
        h.set_drawing(false);
        let Some(s) = h.board().live.get(&id).cloned() else { return };
        if ping {
            h.board().end_live(&id);
            h.redraw_live();
            h.send_ping(s.pts, touch);
            h.ping_done();
            return;
        }
        if sent && !out.is_empty() {
            h.send(Out::Points { id: id.clone(), pts: out });
        }
        h.board().end_live(&id);
        if s.pts.is_empty() {
            h.redraw_live();
            if sent {
                h.send(Out::Undo { id });
            }
            return;
        }
        // Zoomed in, detail is finer: tighten the tolerance to match.
        let pts = geom::simplify(&if touch { geom::steady(&s.pts, bw * zoom) } else { s.pts }, eps);
        let f = Item { id: id.clone(), l: h.pen().me, q: None, c: s.c, w: s.w, pts, txt: None, sh: None, st: Default::default() };
        h.insert_stroke(f.clone());
        h.redraw_live();
        h.board().pending.insert(id.clone(), f.clone());
        h.mine().push(id.clone());
        // Confirmed by `ok`. If the socket is down or still loading the snapshot, the stroke
        // goes out as `s` when `ready` arrives.
        if sent {
            h.send(Out::End { id, c: f.c, w: f.w, pts: f.pts });
        } else if online_ready(h) {
            h.send(Out::Stroke { id, c: f.c, w: f.w, pts: f.pts, txt: None, sh: None, st: Default::default() });
        }
    }

    /// `pointerEnd`, for `pointerup` (`up`), `pointercancel` and `lostpointercapture`.
    pub fn end(&mut self, h: &mut impl Host, e: &Ptr, up: bool) {
        if e.kind == Kind::Pen {
            self.pen_at = h.now();
        }
        if self.touches.shift_remove(&e.id).is_some() && self.pinch.is_some() && self.touches.len() < 2 {
            self.pinch = None;
            h.settle_view();
        }
        if self.drag.is_some_and(|d| d.pointer == e.id) {
            self.drag = None;
            h.settle_view();
            return;
        }
        if self.text_tap == Some(e.id) {
            self.text_tap = None;
            if up {
                self.text_at(h, e);
            }
            return;
        }
        if let Some(g) = self.gest.as_ref().filter(|g| g.pointer() == e.id) {
            if matches!(g, Gest::Marquee { .. } | Gest::MoveSel { .. }) {
                self.select_end(h, up);
            } else {
                self.shape_end(h, up);
            }
            return;
        }
        self.end_stroke(h, e.id);
    }

    /// `visibilitychange` to hidden: a lift can go missing when the app is sent to the
    /// background mid-stroke.
    pub fn hidden(&mut self, h: &mut impl Host) {
        if let Some(p) = self.cur.as_ref().map(|c| c.pointer) {
            self.end_stroke(h, p);
        }
        self.touches.clear();
        self.pinch = None;
        self.drag = None;
        self.text_tap = None;
        self.cancel_shape(h);
        h.settle_view();
    }

    /// `wheel`, not over a panel (the caller checks that and `preventDefault`s).
    pub fn wheel(&self, h: &mut impl Host, w: &Wheel) {
        let Wheel { dx, dy, line, zoom_key, cx, cy } = *w;
        let rect = h.rect();
        let k = if line { 16.0 } else { 1.0 };
        if zoom_key {
            // A trackpad pinch sends small steps; a mouse wheel notch sends 100 or more. Cap a
            // step at about 1.35x.
            let d = (-30f64).max(30f64.min(dy * k));
            h.zoom_at((-d * 0.01).exp(), cx - rect.0, cy - rect.1, false);
        } else {
            let v = h.view();
            let s = v.bw * v.zoom;
            let (z, x, y) = (v.zoom, v.pan_x + (dx * k) / s, v.pan_y + (dy * k) / s);
            h.set_view(z, x, y, false);
        }
    }

    /// Safari `gesturestart`.
    pub fn gesture_start(&mut self, h: &impl Host) {
        self.gesture_zoom = h.view().zoom;
    }

    /// Safari `gesturechange`: a Mac trackpad pinch. `scale` is `e.scale` (NaN when absent).
    pub fn gesture_change(&self, h: &mut impl Host, scale: f64, cx: f64, cy: f64) {
        // fingers on the board are handled as pointers; `!e.scale`
        if !self.touches.is_empty() || scale == 0.0 || scale.is_nan() {
            return;
        }
        // a resting hand, not a trackpad
        if self.cur.is_some() || h.now() - self.last_touch_at < 1000.0 {
            return;
        }
        let rect = h.rect();
        let f = (self.gesture_zoom * scale) / h.view().zoom;
        h.zoom_at(f, cx - rect.0, cy - rect.1, false);
    }

    // ---- text annotations ------------------------------------------------------------------
    // A text annotation is an item in my layer like a stroke: one anchor point (its top left),
    // a font size in board units, and the text.

    /// `openEditor`.
    pub fn open_editor(&mut self, h: &mut impl Host, x: f64, y: f64, old: Option<Item>) {
        let pen = h.pen();
        let ed = Editor {
            x,
            y,
            c: old.as_ref().map_or_else(|| pen.color.clone().unwrap_or_default(), |o| o.c.clone()),
            w: old.as_ref().map_or(pen.width * TEXT_SCALE, |o| o.w),
            old,
        };
        if let Some(o) = &ed.old {
            h.remove_stroke(&o.id); // shown in the box while it is edited
        }
        let text = ed.old.as_ref().and_then(|o| o.txt.clone()).unwrap_or_default();
        h.editor_show(&ed, &text);
        self.editor = Some(ed);
    }

    /// `closeEditor`.
    pub fn close_editor(&mut self, h: &mut impl Host, save: bool) {
        let Some(ed) = self.editor.take() else { return };
        let txt = clean_text(&h.editor_text());
        h.editor_hide();
        if let Some(old) = &ed.old {
            if !save || old.txt.as_deref() == Some(txt.as_str()) {
                h.insert_stroke(old.clone()); // unchanged: put it back
                return;
            }
            // replaced or emptied: the old one goes, as an undo
            if h.board().has(&old.id) {
                h.remove_stroke(&old.id); // a reconnect may have put it back
            }
            h.board().pending.shift_remove(&old.id);
            if online_ready(h) {
                h.send(Out::Undo { id: old.id.clone() });
            } else {
                h.board().undo_queue.push(old.id.clone());
            }
        }
        let me = h.pen().me;
        if !save || js_trim(&txt).is_empty() || me.is_none() {
            return;
        }
        let id = h.new_id();
        let f = Item { id: id.clone(), l: me, q: None, c: ed.c, w: r5(ed.w), pts: vec![r5(ed.x), r5(ed.y), 0.5], txt: Some(txt), sh: None, st: Default::default() };
        h.insert_stroke(f.clone());
        h.board().pending.insert(id.clone(), f.clone());
        h.mine().push(id.clone());
        if online_ready(h) {
            h.send(Out::Stroke { id, c: f.c, w: f.w, pts: f.pts, txt: f.txt, sh: None, st: Default::default() });
        }
    }

    /// `textAt`: a tap with the text tool edits my text under it, or starts a new one there.
    fn text_at(&mut self, h: &mut impl Host, e: &Ptr) {
        let rect = h.rect();
        let (x, y) = h.view().board_point(e.x - rect.0, e.y - rect.1);
        let me = h.pen().me;
        let n = h.board().strokes.len();
        for i in (0..n).rev() {
            let s = &h.board().strokes[i];
            if s.txt.is_none() || s.l != me {
                continue;
            }
            let s = s.clone();
            if h.hidden(s.l) {
                continue;
            }
            let b = h.bbox(&s);
            if x >= b[0] && x <= b[2] && y >= b[1] && y <= b[3] {
                // a text item read from damaged storage may carry fewer than two numbers
                let [ax, ay] = [s.pts.first(), s.pts.get(1)].map(|v| v.copied().unwrap_or(0.0));
                return self.open_editor(h, ax, ay, Some(s));
            }
        }
        let hh = h.pen().width * TEXT_SCALE * LINE_HEIGHT;
        self.open_editor(h, x, clamp(y - hh / 2.0, WY, WY + WH - hh), None);
    }

    /// `undo`: take back my latest item still on the board.
    pub fn undo(&mut self, h: &mut impl Host) {
        if !h.pen().edit {
            return;
        }
        if self.editor.is_some() {
            self.close_editor(h, false); // undo while typing cancels the text box
            return;
        }
        if !self.sel.is_empty() {
            self.sel.clear();
            self.show_sel(h, Overlay::default());
        }
        while let Some(id) = h.mine().pop() {
            if let Some(step) = self.steps.remove(&id) {
                if self.undo_step(h, step) {
                    return;
                }
                continue;
            }
            if !h.board().has(&id) {
                continue; // already gone (layer deleted, refused)
            }
            h.remove_stroke(&id);
            h.board().pending.shift_remove(&id);
            if online_ready(h) {
                h.send(Out::Undo { id });
            } else if !h.net().local {
                h.board().undo_queue.push(id);
            }
            h.save_local();
            return;
        }
    }

    /// A window `keydown`, after [`key`] read it.
    pub fn key(&mut self, h: &mut impl Host, k: Key) {
        match k {
            Key::Undo => self.undo(h),
            Key::Space => self.space_down = true,
            Key::Delete => self.delete_sel(h),
            Key::Group => self.group_sel(h),
            Key::Ungroup => self.ungroup_sel(h),
            _ => {}
        }
    }

    /// `keyup` of Space, or the window losing focus.
    pub fn space_up(&mut self) {
        self.space_down = false;
    }
}

// ---- shapes and lines (A-166) --------------------------------------------------------------
// A shape is an item whose two points are its box corners; a line is one whose two points are
// its ends. Both go out as one `s` frame when the drag ends, like a text annotation.

impl Input {
    /// The pointer as a board point.
    fn board_at(h: &impl Host, e: &Ptr) -> (f64, f64) {
        let rect = h.rect();
        h.view().board_point(e.x - rect.0, e.y - rect.1)
    }

    /// Board units per screen px.
    fn per_px(h: &impl Host) -> f64 {
        let v = h.view();
        1.0 / (v.bw * v.zoom)
    }

    /// A line end at `(x, y)`: on the anchor it snaps to, if any (never on `except`, the
    /// node the other end is on), with the dots of the shape it is near.
    fn end_at(&mut self, h: &mut impl Host, x: f64, y: f64, except: Option<&str>) -> (End, Vec<(f64, f64, bool)>) {
        let (px, now) = (Self::per_px(h), h.now());
        let Some((n, nd)) = h.scene().top_node(x, y, shape::NEAR_PX * px, except).map(|(n, nd)| (n.to_string(), *nd)) else {
            self.hover.step(None, None, now);
            return (End { x, y, at: None }, vec![]);
        };
        let (sp, ssp) = (shape::SNAP_PX * px, shape::SUB_SNAP_PX * px);
        let resting = shape::snap(&nd, self.hover.side_of(&n), x, y, sp, ssp, false);
        self.hover.step(Some(&n), resting, now);
        let exp = self.hover.side_of(&n);
        let on = shape::snap(&nd, exp, x, y, sp, ssp, true);
        let dots = shape::anchors(&nd, exp).into_iter().map(|(s, t, (ax, ay))| (ax, ay, on == Some((s, t)))).collect();
        let end = match on {
            Some((s, t)) => {
                let (ax, ay) = shape::anchor_point(nd.kind, &nd.bb, s, t);
                End { x: ax, y: ay, at: Some(Anchor { n, s, t: shape::round_t(t) }) }
            }
            None => End { x, y, at: None },
        };
        (end, dots)
    }

    fn show(&mut self, h: &mut impl Host, o: Option<Overlay>) {
        if o.is_none() && !self.shown {
            return;
        }
        self.shown = o.is_some();
        h.overlay(o);
    }

    /// The bucket: the top shape of mine under the point gets `color` as its fill, one change
    /// and one undo; the same colour again takes the fill off. Empty board does nothing.
    fn bucket(&mut self, h: &mut impl Host, e: &Ptr, color: &str) {
        let (x, y) = Self::board_at(h, e);
        let me = h.pen().me;
        let hit = h.board().strokes.iter().rev().find(|s| {
            s.l == me
                && matches!(s.sh, Some(Shape::Box { .. }))
                && shape::norm_box(&s.pts).is_some_and(|b| x >= b[0] && x <= b[2] && y >= b[1] && y <= b[3])
        }).cloned();
        let Some(old) = hit.filter(|s| !h.hidden(s.l)) else { return };
        let c = color.to_ascii_lowercase();
        let fc = if old.st.fc.as_deref() == Some(c.as_str()) { None } else { shape::rgb(&c).map(|_| c) };
        if fc == old.st.fc {
            return;
        }
        let id = h.new_id();
        let new = Item { id, q: None, st: Style { fc, ..old.st.clone() }, ..old.clone() };
        self.edit(h, vec![old], vec![new]);
    }

    /// (x, y) on the nearest grid point when snap to grid is on.
    fn snap(h: &mut impl Host, x: f64, y: f64) -> (f64, f64) {
        if !h.pen().snap {
            return (x, y);
        }
        let s = h.view().grid_step();
        ((x / s).round() * s, (y / s).round() * s)
    }

    /// A move by (dx, dy) of something whose corner is (x0, y0), adjusted so the corner lands
    /// on the grid when snap is on.
    fn snap_by(h: &mut impl Host, x0: f64, y0: f64, dx: f64, dy: f64) -> (f64, f64) {
        let (x, y) = Self::snap(h, x0 + dx, y0 + dy);
        (x - x0, y - y0)
    }

    /// A line end: an anchor wins, a free end snaps.
    fn snap_end(h: &mut impl Host, e: End) -> End {
        if e.at.is_some() {
            return e;
        }
        let (x, y) = Self::snap(h, e.x, e.y);
        End { x, y, at: None }
    }

    /// The line tool over the board with nothing pressed: the anchors of the shape it is near.
    fn line_hover(&mut self, h: &mut impl Host, e: &Ptr) {
        let pen = h.pen();
        if pen.tool != Tool::Line || viewer(&pen) || e.kind == Kind::Touch {
            return self.show(h, None);
        }
        let (x, y) = Self::board_at(h, e);
        let (_, dots) = self.end_at(h, x, y, None);
        self.show(h, (!dots.is_empty()).then(|| Overlay { ghosts: vec![], dots, ..Overlay::default() }));
    }

    fn shape_down(&mut self, h: &mut impl Host, e: &Ptr, pen: &Pen) {
        let (x, y) = Self::board_at(h, e);
        let pointer = e.id;
        match pen.tool {
            Tool::Line => {
                let (a, dots) = self.end_at(h, x, y, None);
                let a = Self::snap_end(h, a);
                self.gest = Some(Gest::Line { pointer, a: a.clone(), b: a });
                self.show(h, Some(Overlay { ghosts: vec![], dots, ..Overlay::default() }));
            }
            Tool::Shape(kind) => {
                let m = 4.0 * Self::per_px(h);
                let me = pen.me;
                let mut hit = None;
                for s in h.board().strokes.iter().rev() {
                    if s.l != me || !matches!(s.sh, Some(Shape::Box { .. })) {
                        continue;
                    }
                    if let Some([x0, y0, x1, y1]) = shape::norm_box(&s.pts) {
                        if x >= x0 - m && x <= x1 + m && y >= y0 - m && y <= y1 + m {
                            hit = Some(s.clone());
                            break;
                        }
                    }
                }
                self.gest = Some(match hit.filter(|s| !h.hidden(s.l)) {
                    Some(old) => Gest::MoveBox { pointer, old, sx: x, sy: y, dx: 0.0, dy: 0.0 },
                    None => {
                        let (x, y) = Self::snap(h, x, y);
                        Gest::NewBox { pointer, kind, x0: x, y0: y, x1: x, y1: y }
                    }
                });
            }
            _ => {}
        }
    }

    fn shape_move(&mut self, h: &mut impl Host, e: &Ptr) {
        let (x, y) = Self::board_at(h, e);
        let Some(mut g) = self.gest.take() else { return };
        let pen = h.pen();
        let bw = h.view().bw;
        let w = h.view().pen_w(pen.width);
        let c = pen.color.clone().unwrap_or_default();
        let o = match &mut g {
            Gest::NewBox { kind, x0, y0, x1, y1, .. } => {
                (*x1, *y1) = Self::snap(h, x, y);
                let bb = [x0.min(*x1), y0.min(*y1), x0.max(*x1), y0.max(*y1)];
                Overlay { ghosts: vec![(c, shape::box_ops(*kind, &bb, pen.fill, None, pen.style.d, w, bw))], ..Overlay::default() }
            }
            Gest::MoveBox { old, sx, sy, dx, dy, .. } => {
                (*dx, *dy) = (x - *sx, y - *sy);
                if let Some([x0, y0, ..]) = shape::norm_box(&old.pts) {
                    (*dx, *dy) = Self::snap_by(h, x0, y0, *dx, *dy);
                }
                self.move_ghost(h, old, *dx, *dy)
            }
            Gest::Line { a, b, .. } => {
                let except = a.at.as_ref().map(|e| e.n.clone());
                let (end, dots) = self.end_at(h, x, y, except.as_deref());
                *b = Self::snap_end(h, end);
                let ops = shape::line_ops(&[a.x, a.y, b.x, b.y], w, pen.heads, &[], pen.style.d, bw);
                Overlay { ghosts: vec![(c, ops)], dots, ..Overlay::default() }
            }
            Gest::Marquee { .. } | Gest::MoveSel { .. } => Overlay::default(),
        };
        self.gest = Some(g);
        self.show(h, Some(o));
    }

    /// The moved box and every line on it, where they would be.
    fn move_ghost(&self, h: &mut impl Host, old: &Item, dx: f64, dy: f64) -> Overlay {
        let bw = h.view().bw;
        let Some(Shape::Box { kind, fill, n }) = &old.sh else { return Overlay::default() };
        let Some([x0, y0, x1, y1]) = shape::norm_box(&old.pts) else { return Overlay::default() };
        let nd = Node { kind: *kind, bb: [x0 + dx, y0 + dy, x1 + dx, y1 + dy] };
        let mut ghosts = vec![(old.c.clone(), shape::box_ops(*kind, &nd.bb, *fill, old.st.fc.as_deref(), old.st.d, old.w, bw))];
        let lines: Vec<Item> = h
            .board()
            .strokes
            .iter()
            .filter(|s| s.sh.as_ref().is_some_and(|sh| sh.touches(n)))
            .cloned()
            .collect();
        for it in lines {
            if h.hidden(it.l) {
                continue;
            }
            if let Some(seg) = h.scene().resolve_with(&it, Some((n, &nd))) {
                ghosts.push((it.c.clone(), shape::line_ops(&seg.p, seg.w, seg.h, &[], it.st.d, bw)));
            }
        }
        Overlay { ghosts, ..Overlay::default() }
    }

    fn shape_end(&mut self, h: &mut impl Host, up: bool) {
        let g = self.gest.take();
        self.hover = Hover::default();
        self.show(h, None);
        let (Some(g), true) = (g, up) else { return };
        let pen = h.pen();
        let px = Self::per_px(h);
        let w = h.view().pen_w(pen.width);
        let c = pen.color.clone().unwrap_or_default();
        let pt = |x: f64, y: f64| [r5(x), r5(y), 0.5];
        match g {
            Gest::NewBox { kind, x0, y0, x1, y1, .. } => {
                if (x1 - x0).abs().max((y1 - y0).abs()) < 4.0 * px {
                    return; // a click, not a drag
                }
                let n = h.new_id();
                let pts = [pt(x0, y0), pt(x1, y1)].concat();
                self.commit(h, c, w, pts, Shape::Box { kind, fill: pen.fill, n });
            }
            Gest::MoveBox { old, dx, dy, .. } => {
                if dx.abs().max(dy.abs()) < 2.0 * px || old.pts.len() < 6 {
                    return;
                }
                let pts = [pt(old.pts[0] + dx, old.pts[1] + dy), pt(old.pts[3] + dx, old.pts[4] + dy)].concat();
                let id = h.new_id();
                let moved = Item { id, q: None, pts, ..old.clone() };
                self.edit(h, vec![old], vec![moved]);
            }
            Gest::Line { a, b, .. } => {
                if (b.x - a.x).hypot(b.y - a.y) < 4.0 * px {
                    return;
                }
                let pts = [pt(a.x, a.y), pt(b.x, b.y)].concat();
                // the relay refuses a line with both ends on one shape (end_at never makes one)
                let bt = b.at.filter(|bt| a.at.as_ref().is_none_or(|at| at.n != bt.n));
                self.commit(h, c, w, pts, Shape::Line { h: pen.heads, a: a.at, b: bt });
            }
            Gest::Marquee { .. } | Gest::MoveSel { .. } => {}
        }
    }

    /// A finished shape or line on my layer, sent as one `s`.
    fn commit(&mut self, h: &mut impl Host, c: String, w: f64, pts: Vec<f64>, sh: Shape) {
        let me = h.pen().me;
        if me.is_none() {
            return;
        }
        let st = h.pen().style.clone();
        let id = h.new_id();
        let f = Item { id: id.clone(), l: me, q: None, c, w, pts, txt: None, sh: Some(sh), st };
        self.change(h, &[], vec![f]);
        h.mine().push(id);
    }

    /// Replace my items `old` with `new` as ONE change: one `x` to the relay (applied whole or
    /// not at all), one undo step. `new` items need fresh ids; a box keeps its name, so its
    /// arrows stay on it. Nothing happens when `old` holds an item that is not mine.
    pub fn edit(&mut self, h: &mut impl Host, old: Vec<Item>, new: Vec<Item>) {
        let me = h.pen().me;
        if me.is_none() || old.iter().any(|o| o.l != me) || (old.is_empty() && new.is_empty()) {
            return;
        }
        let gone: Vec<String> = old.iter().map(|o| o.id.clone()).collect();
        let ids: Vec<String> = new.iter().map(|n| n.id.clone()).collect();
        self.change(h, &gone, new);
        // the step goes under its last new id, or a fresh key when it only removes
        let key = ids.last().cloned().unwrap_or_else(|| format!("~{}", h.new_id()));
        // `mine` keeps the old ids below the step: undo renames them when it puts them back,
        // and they stand for themselves again if the relay refuses the change
        self.steps.insert(key.clone(), Step { gone: old, new: ids });
        h.mine().push(key);
    }

    /// Take `gone` off the board and put `new` on, and tell the relay: one frame for one
    /// part, an `x` for more. Offline, removals queue and new items wait in `pending` as usual.
    fn change(&mut self, h: &mut impl Host, gone: &[String], new: Vec<Item>) {
        for id in gone {
            if h.board().has(id) {
                h.remove_stroke(id);
            }
            h.board().pending.shift_remove(id);
        }
        for f in &new {
            h.insert_stroke(f.clone());
            h.board().pending.insert(f.id.clone(), f.clone());
        }
        if online_ready(h) {
            let mut parts: Vec<Out> = gone.iter().map(|id| Out::Undo { id: id.clone() }).collect();
            parts.extend(new.iter().map(out_of));
            if parts.len() == 1 {
                h.send(parts.pop().expect("one part"));
            } else if !parts.is_empty() {
                h.send(Out::Batch(parts));
            }
        } else if !h.net().local {
            h.board().undo_queue.extend(gone.iter().cloned());
        }
        h.save_local();
    }

    /// Undo one step: its new items go and its old ones come back, as one change. `false`
    /// when there is nothing left of it to undo (its items were refused or deleted).
    fn undo_step(&mut self, h: &mut impl Host, step: Step) -> bool {
        let live: Vec<String> = step.new.iter().filter(|id| h.board().has(id)).cloned().collect();
        // refused: the relay gave the old items back already
        let refused = (!step.new.is_empty() && live.is_empty()) || step.gone.iter().any(|g| h.board().has(&g.id));
        if refused {
            return false;
        }
        let mut back = Vec::new();
        for g in step.gone {
            let id = h.new_id();
            self.renamed(h, &g.id, &id);
            back.push(Item { id, q: None, ..g });
        }
        self.change(h, &live, back);
        true
    }

    /// An item came back under a new id: older undo entries for it now mean the new one.
    fn renamed(&mut self, h: &mut impl Host, old: &str, new: &str) {
        for m in h.mine().iter_mut() {
            if m == old {
                *m = new.to_string();
            }
        }
        if let Some(st) = self.steps.remove(old) {
            self.steps.insert(new.to_string(), st);
        }
        for st in self.steps.values_mut() {
            for n in st.new.iter_mut() {
                if n == old {
                    *n = new.to_string();
                }
            }
        }
    }

    /// Escape, a tool change or the page hiding: drop the gesture and its overlay.
    pub fn cancel_shape(&mut self, h: &mut impl Host) {
        self.gest = None;
        self.hover = Hover::default();
        self.show(h, None);
    }
}

// ---- selection and groups (A-166 items 12, 17) ---------------------------------------------
// The select tool picks my items. A click picks the unit under it: the item, or the outermost
// group it is in below the groups entered by double-click. Moving, deleting, grouping and
// ungrouping are each ONE edit: one `x` to the relay and one undo step.

/// A click picks this much past an item's edge, screen px.
const PICK_PX: f64 = 6.0;
/// Two clicks on one item this close together (ms) are a double-click.
const DOUBLE_MS: f64 = 400.0;

/// Distance from `(x, y)` to the segment `a`-`b`.
fn seg_dist(x: f64, y: f64, a: (f64, f64), b: (f64, f64)) -> f64 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let l = dx * dx + dy * dy;
    let t = if l == 0.0 { 0.0 } else { (((x - a.0) * dx + (y - a.1) * dy) / l).clamp(0.0, 1.0) };
    (x - a.0 - t * dx).hypot(y - a.1 - t * dy)
}

/// `pts` (x, y, p triples) moved by `(dx, dy)`.
fn shifted(pts: &[f64], dx: f64, dy: f64) -> Vec<f64> {
    pts.chunks(3).flat_map(|c| [r5(c[0] + dx), r5(c.get(1).copied().unwrap_or(0.0) + dy), c.get(2).copied().unwrap_or(0.5)]).collect()
}

/// What a click picks: the group `g[depth]` when the item is deeper than `path`, else the item.
/// `None` when the item is not inside `path` at all.
fn unit_of(it: &Item, path: &[String]) -> Option<String> {
    let g = &it.st.g;
    if !g.starts_with(path) {
        return None;
    }
    Some(g.get(path.len()).cloned().unwrap_or_else(|| it.id.clone()))
}

impl Input {
    /// Whether the select tool may pick `s`: mine, visible, not an eraser stroke.
    fn pickable(h: &impl Host, s: &Item, me: Option<Layer>) -> bool {
        s.l == me && me.is_some() && s.c != ERASER_COLOR && !h.hidden(s.l)
    }

    /// My topmost item under board point `(x, y)`.
    fn pick(h: &mut impl Host, x: f64, y: f64) -> Option<Item> {
        let me = h.pen().me;
        let m = PICK_PX * Self::per_px(h);
        let n = h.board().strokes.len();
        for i in (0..n).rev() {
            let s = h.board().strokes[i].clone();
            if !Self::pickable(h, &s, me) {
                continue;
            }
            let hit = match &s.sh {
                Some(Shape::Line { .. }) => {
                    let p = h.scene().seg(&s.id).map(|sg| sg.p);
                    p.is_some_and(|p| seg_dist(x, y, (p[0], p[1]), (p[2], p[3])) <= m + s.w / 2.0)
                }
                Some(Shape::Box { .. }) => {
                    let b = h.bbox(&s);
                    x >= b[0] - m && x <= b[2] + m && y >= b[1] - m && y <= b[3] + m
                }
                None if s.txt.is_some() => {
                    let b = h.bbox(&s);
                    x >= b[0] && x <= b[2] && y >= b[1] && y <= b[3]
                }
                None => {
                    let xy: Vec<(f64, f64)> = s.pts.chunks(3).filter(|c| c.len() >= 2).map(|c| (c[0], c[1])).collect();
                    let tol = m + s.w / 2.0;
                    match xy.len() {
                        0 => false,
                        1 => (x - xy[0].0).hypot(y - xy[0].1) <= tol,
                        _ => xy.windows(2).any(|w| seg_dist(x, y, w[0], w[1]) <= tol),
                    }
                }
            };
            if hit {
                return Some(s);
            }
        }
        None
    }

    /// Every one of my items in the unit `u` (a group id or an item id).
    fn members(h: &mut impl Host, u: &str) -> Vec<String> {
        let me = h.pen().me;
        let all: Vec<Item> = h.board().strokes.iter().filter(|s| s.l == me).cloned().collect();
        let mut ids: Vec<String> = all.iter().filter(|s| s.st.g.iter().any(|g| g == u)).map(|s| s.id.clone()).collect();
        if ids.is_empty() && all.iter().any(|s| s.id == u) {
            ids.push(u.to_string());
        }
        ids
    }

    /// The selected items still on the board, in board order.
    fn sel_items(&self, h: &mut impl Host) -> Vec<Item> {
        h.board().strokes.iter().filter(|s| self.sel.contains(&s.id)).cloned().collect()
    }

    /// Show the selection's box (and `extra`), and tell the toolbar what Group can do.
    /// The top-left of the selection: box corners and line ends, else outline boxes.
    fn sel_corner(&self, h: &mut impl Host) -> Option<(f64, f64)> {
        let mut c: Option<(f64, f64)> = None;
        for s in self.sel_items(h) {
            let (x, y) = match &s.sh {
                Some(Shape::Box { .. }) => match shape::norm_box(&s.pts) {
                    Some(b) => (b[0], b[1]),
                    None => continue,
                },
                Some(Shape::Line { .. }) => match h.scene().seg(&s.id).map(|sg| sg.p) {
                    Some(p) => (p[0].min(p[2]), p[1].min(p[3])),
                    None => continue,
                },
                _ => {
                    let b = h.bbox(&s);
                    (b[0], b[1])
                }
            };
            c = Some(c.map_or((x, y), |(a, b)| (a.min(x), b.min(y))));
        }
        c
    }

    fn show_sel(&mut self, h: &mut impl Host, extra: Overlay) {
        let items = self.sel_items(h);
        self.sel.retain(|id| items.iter().any(|s| &s.id == id));
        let mut bb: Option<[f64; 4]> = None;
        for s in &items {
            let b = match &s.sh {
                Some(Shape::Line { .. }) => match h.scene().seg(&s.id).map(|sg| sg.p) {
                    Some(p) => [p[0].min(p[2]), p[1].min(p[3]), p[0].max(p[2]), p[1].max(p[3])],
                    None => h.bbox(s),
                },
                _ => h.bbox(s),
            };
            bb = Some(match bb {
                None => b,
                Some(a) => [a[0].min(b[0]), a[1].min(b[1]), a[2].max(b[2]), a[3].max(b[3])],
            });
        }
        let units = self.units(&items);
        let can_group = units.len() >= 2;
        let can_ungroup = items.iter().any(|s| s.st.g.len() > self.group_path.len());
        h.sel_changed(can_group, can_ungroup);
        let o = Overlay { sel: if extra.ghosts.is_empty() { bb } else { None }, ..extra };
        let empty = o.ghosts.is_empty() && o.dots.is_empty() && o.sel.is_none() && o.marquee.is_none();
        self.show(h, (!empty).then_some(o));
    }

    /// The distinct units (groups below the entered path, or loose items) in `items`.
    fn units(&self, items: &[Item]) -> Vec<String> {
        let mut u: Vec<String> = vec![];
        for s in items {
            let k = unit_of(s, &self.group_path).unwrap_or_else(|| s.id.clone());
            if !u.contains(&k) {
                u.push(k);
            }
        }
        u
    }

    /// Clear the selection and leave any entered group (Escape, a tool change).
    pub fn deselect(&mut self, h: &mut impl Host) {
        let had = !self.sel.is_empty() || !self.group_path.is_empty();
        self.sel.clear();
        self.group_path.clear();
        self.last_click = None;
        if had {
            self.show_sel(h, Overlay::default());
        }
    }

    fn select_down(&mut self, h: &mut impl Host, e: &Ptr) {
        let (x, y) = Self::board_at(h, e);
        let pointer = e.id;
        let now = h.now();
        let Some(hit) = Self::pick(h, x, y) else {
            let base = if e.shift { self.sel.clone() } else { vec![] };
            if !e.shift {
                self.sel.clear();
                self.group_path.clear();
            }
            self.last_click = None;
            self.gest = Some(Gest::Marquee { pointer, base, x0: x, y0: y, x1: x, y1: y });
            return self.show_sel(h, Overlay::default());
        };
        // a click outside the entered group leaves it
        if unit_of(&hit, &self.group_path).is_none() {
            self.group_path.clear();
        }
        let double = self.last_click.as_ref().is_some_and(|(t, id)| now - t < DOUBLE_MS && *id == hit.id);
        self.last_click = Some((now, hit.id.clone()));
        let mut unit = unit_of(&hit, &self.group_path).unwrap_or_else(|| hit.id.clone());
        if double && unit != hit.id {
            // enter the group: the next click picks inside it
            self.group_path.push(unit);
            unit = unit_of(&hit, &self.group_path).unwrap_or_else(|| hit.id.clone());
            self.sel = Self::members(h, &unit);
            self.last_click = None;
        } else if e.shift {
            let ids = Self::members(h, &unit);
            if ids.iter().all(|i| self.sel.contains(i)) {
                self.sel.retain(|i| !ids.contains(i));
            } else {
                self.sel.extend(ids.into_iter().filter(|i| !self.sel.contains(i)).collect::<Vec<_>>());
            }
            return self.show_sel(h, Overlay::default());
        } else if !self.sel.contains(&hit.id) {
            self.sel = Self::members(h, &unit);
        }
        let (ox, oy) = self.sel_corner(h).unwrap_or((x, y));
        self.gest = Some(Gest::MoveSel { pointer, sx: x, sy: y, dx: 0.0, dy: 0.0, ox, oy });
        self.show_sel(h, Overlay::default());
    }

    fn select_move(&mut self, h: &mut impl Host, e: &Ptr) {
        let (x, y) = Self::board_at(h, e);
        let Some(mut g) = self.gest.take() else { return };
        let extra = match &mut g {
            Gest::Marquee { base, x0, y0, x1, y1, .. } => {
                (*x1, *y1) = (x, y);
                let r = [x0.min(*x1), y0.min(*y1), x0.max(*x1), y0.max(*y1)];
                self.sel = base.clone();
                for id in self.in_marquee(h, r) {
                    if !self.sel.contains(&id) {
                        self.sel.push(id);
                    }
                }
                Overlay { marquee: Some(r), ..Overlay::default() }
            }
            Gest::MoveSel { sx, sy, dx, dy, ox, oy, .. } => {
                (*dx, *dy) = Self::snap_by(h, *ox, *oy, x - *sx, y - *sy);
                self.sel_ghost(h, *dx, *dy)
            }
            _ => Overlay::default(),
        };
        self.gest = Some(g);
        self.show_sel(h, extra);
    }

    /// The members of every unit wholly inside `r`.
    fn in_marquee(&self, h: &mut impl Host, r: [f64; 4]) -> Vec<String> {
        let me = h.pen().me;
        let all: Vec<Item> = h.board().strokes.clone();
        let mut inside: HashMap<String, bool> = HashMap::new();
        let mut order: Vec<String> = vec![];
        for s in &all {
            if !Self::pickable(h, s, me) {
                continue;
            }
            let Some(u) = unit_of(s, &self.group_path) else { continue };
            let b = match &s.sh {
                Some(Shape::Line { .. }) => match h.scene().seg(&s.id).map(|sg| sg.p) {
                    Some(p) => [p[0].min(p[2]), p[1].min(p[3]), p[0].max(p[2]), p[1].max(p[3])],
                    None => continue,
                },
                _ => h.bbox(s),
            };
            let ok = b[0] >= r[0] && b[1] >= r[1] && b[2] <= r[2] && b[3] <= r[3];
            let e = inside.entry(u.clone()).or_insert_with(|| {
                order.push(u.clone());
                true
            });
            *e &= ok;
        }
        let mut ids = vec![];
        for u in order {
            if inside[&u] {
                ids.extend(Self::members(h, &u));
            }
        }
        ids
    }

    /// The selection moved by `(dx, dy)`, with every line on a moved shape following.
    fn sel_ghost(&self, h: &mut impl Host, dx: f64, dy: f64) -> Overlay {
        let bw = h.view().bw;
        let items = self.sel_items(h);
        let mut moved: HashMap<String, Node> = HashMap::new();
        let mut ghosts = vec![];
        for s in &items {
            match &s.sh {
                Some(Shape::Box { kind, fill, n }) => {
                    let Some([x0, y0, x1, y1]) = shape::norm_box(&s.pts) else { continue };
                    let nd = Node { kind: *kind, bb: [x0 + dx, y0 + dy, x1 + dx, y1 + dy] };
                    ghosts.push((s.c.clone(), shape::box_ops(*kind, &nd.bb, *fill, s.st.fc.as_deref(), s.st.d, s.w, bw)));
                    moved.insert(n.clone(), nd);
                }
                Some(Shape::Line { .. }) => {}
                None if s.txt.is_some() => {
                    let b = h.bbox(s);
                    ghosts.push((s.c.clone(), shape::box_ops(BoxKind::Rect, &[b[0] + dx, b[1] + dy, b[2] + dx, b[3] + dy], false, None, shape::Dash::Solid, 0.001, bw)));
                }
                None => ghosts.push((s.c.clone(), geom::full(&shifted(&s.pts, dx, dy), s.w, bw))),
            }
        }
        let lines: Vec<Item> = h
            .board()
            .strokes
            .iter()
            .filter(|s| s.sh.as_ref().is_some_and(|sh| sh.is_line() && (self.sel.contains(&s.id) || moved.keys().any(|n| sh.touches(n)))))
            .cloned()
            .collect();
        for it in lines {
            if h.hidden(it.l) {
                continue;
            }
            let it = if self.sel.contains(&it.id) { Item { pts: shifted(&it.pts, dx, dy), ..it } } else { it };
            if let Some(seg) = h.scene().resolve_in(&it, |n| moved.get(n).copied()) {
                ghosts.push((it.c.clone(), shape::line_ops(&seg.p, seg.w, seg.h, &[], it.st.d, bw)));
            }
        }
        Overlay { ghosts, ..Overlay::default() }
    }

    fn select_end(&mut self, h: &mut impl Host, up: bool) {
        let g = self.gest.take();
        if let (Some(Gest::MoveSel { dx, dy, .. }), true) = (g, up) {
            if dx.abs().max(dy.abs()) >= 2.0 * Self::per_px(h) {
                let old = self.sel_items(h);
                let new: Vec<Item> = old.iter().map(|s| Item { id: h.new_id(), q: None, pts: shifted(&s.pts, dx, dy), ..s.clone() }).collect();
                self.replace_sel(h, old, new);
            }
        }
        self.show_sel(h, Overlay::default());
    }

    /// One edit from `old` to `new`; the selection becomes `new`.
    fn replace_sel(&mut self, h: &mut impl Host, old: Vec<Item>, new: Vec<Item>) {
        let keep: Vec<String> = self.sel.iter().filter(|id| !old.iter().any(|o| &o.id == *id)).cloned().collect();
        let ids: Vec<String> = new.iter().map(|n| n.id.clone()).collect();
        self.edit(h, old, new);
        self.sel = keep.into_iter().chain(ids).collect();
    }

    /// Delete or Backspace: the selection goes, as one edit.
    fn delete_sel(&mut self, h: &mut impl Host) {
        let old = self.sel_items(h);
        if old.is_empty() || self.gest.is_some() {
            return;
        }
        self.replace_sel(h, old, vec![]);
        self.sel.clear();
        self.show_sel(h, Overlay::default());
    }

    /// Ctrl/Cmd+G: the selected units become one group, inside the entered one.
    pub fn group_sel(&mut self, h: &mut impl Host) {
        let old = self.sel_items(h);
        if self.units(&old).len() < 2 || self.gest.is_some() {
            return;
        }
        let at = self.group_path.len();
        if old.iter().any(|s| s.st.g.len() >= shape::MAX_GROUP_DEPTH || !s.st.g.starts_with(&self.group_path)) {
            return;
        }
        let gid = h.new_id();
        let new: Vec<Item> = old
            .iter()
            .map(|s| {
                let mut st = s.st.clone();
                st.g.insert(at, gid.clone());
                Item { id: h.new_id(), q: None, st, ..s.clone() }
            })
            .collect();
        self.replace_sel(h, old, new);
        self.show_sel(h, Overlay::default());
    }

    /// Shift+Ctrl/Cmd+G: the selected groups below the entered one come apart one level.
    pub fn ungroup_sel(&mut self, h: &mut impl Host) {
        let at = self.group_path.len();
        let old: Vec<Item> = self.sel_items(h).into_iter().filter(|s| s.st.g.len() > at).collect();
        if old.is_empty() || self.gest.is_some() {
            return;
        }
        let new: Vec<Item> = old
            .iter()
            .map(|s| {
                let mut st = s.st.clone();
                st.g.remove(at);
                Item { id: h.new_id(), q: None, st, ..s.clone() }
            })
            .collect();
        self.replace_sel(h, old, new);
        self.show_sel(h, Overlay::default());
    }

    /// Draw the selection's box again (after something cleared the overlay).
    pub fn sel_redraw(&mut self, h: &mut impl Host) {
        if !self.sel.is_empty() && self.gest.is_none() {
            self.show_sel(h, Overlay::default());
        }
    }

    /// The board changed under the selection (a refusal, a layer deleted): drop what is gone.
    pub fn sel_check(&mut self, h: &mut impl Host) {
        if self.sel.is_empty() || self.gest.is_some() {
            return;
        }
        let n = self.sel.len();
        let on: Vec<String> = self.sel.iter().filter(|id| h.board().has(id)).cloned().collect();
        if on.len() != n {
            self.sel = on;
            self.show_sel(h, Overlay::default());
        }
    }
}

/// The `s` frame that sends item `f`.
pub fn out_of(f: &Item) -> Out {
    Out::Stroke { id: f.id.clone(), c: f.c.clone(), w: f.w, pts: f.pts.clone(), txt: f.txt.clone(), sh: f.sh.clone(), st: f.st.clone() }
}

/// A `wheel` event.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Wheel {
    pub dx: f64,
    pub dy: f64,
    /// `deltaMode === 1`: the deltas are lines.
    pub line: bool,
    /// Ctrl or Cmd held, or a trackpad pinch.
    pub zoom_key: bool,
    /// The pointer, client px.
    pub cx: f64,
    pub cy: f64,
}

/// What a window `keydown` asks for.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Key {
    None,
    Undo,
    /// Escape: close the open panel.
    ClosePanel,
    ZoomStep(f64),
    ZoomHome,
    PageStep(f64, f64),
    Space,
    /// Delete or Backspace: remove the selection.
    Delete,
    /// Ctrl/Cmd+G: group the selection.
    Group,
    /// Shift+Ctrl/Cmd+G: take the selection out of its group.
    Ungroup,
}

impl Key {
    /// Whether the handler calls `preventDefault`.
    pub fn prevents(self) -> bool {
        matches!(self, Key::Undo | Key::Space | Key::Group | Key::Ungroup)
    }
}

/// The window `keydown` handler's choice. `in_input`: the target is an `<input>`.
pub fn key(key: &str, ctrl: bool, meta: bool, shift: bool, alt: bool, in_input: bool) -> Key {
    if (ctrl || meta) && !shift && !alt && key.to_lowercase() == "z" {
        return if in_input { Key::None } else { Key::Undo };
    }
    if (ctrl || meta) && !alt && key.to_lowercase() == "g" {
        return match (in_input, shift) {
            (true, _) => Key::None,
            (false, false) => Key::Group,
            (false, true) => Key::Ungroup,
        };
    }
    if key == "Escape" {
        return Key::ClosePanel;
    }
    if in_input || ctrl || meta || alt {
        return Key::None;
    }
    match key {
        "+" | "=" => Key::ZoomStep(1.5),
        "-" => Key::ZoomStep(1.0 / 1.5),
        "0" => Key::ZoomHome,
        "ArrowLeft" => Key::PageStep(-1.0, 0.0),
        "ArrowRight" => Key::PageStep(1.0, 0.0),
        "ArrowUp" => Key::PageStep(0.0, -1.0),
        "ArrowDown" => Key::PageStep(0.0, 1.0),
        " " => Key::Space,
        "Delete" | "Backspace" if !shift => Key::Delete,
        _ => Key::None,
    }
}

/// The text box's `keydown`: `Some(save)` closes it (and `preventDefault`s). The event never
/// reaches the window handler.
pub fn editor_key(key: &str, shift: bool, composing: bool) -> Option<bool> {
    match key {
        "Enter" if !shift && !composing => Some(true),
        "Escape" => Some(false),
        _ => None,
    }
}

/// `placeEditor` without the height, which needs the element's `scrollHeight`: left, top,
/// font size and width in CSS px. The height is `max(scrollHeight, font * LINE_HEIGHT)`.
pub fn editor_box(ed: &Editor, v: &View) -> (f64, f64, f64, f64) {
    let k = v.bw * v.zoom;
    let px = ed.w * k;
    let left = (ed.x - v.pan_x) * k;
    let top = (ed.y - v.pan_y) * k;
    let width = 60f64.max((crate::view::WX + crate::view::WW - v.pan_x) * k - left - 2.0);
    (left, top, px, width)
}

/// JavaScript's `\s` (WhiteSpace and LineTerminator), which `trim` also strips. Not Rust's
/// `char::is_whitespace`: that has U+0085 and lacks U+FEFF.
pub fn js_space(c: char) -> bool {
    matches!(c,
        '\t' | '\n' | '\u{b}' | '\u{c}' | '\r' | ' ' | '\u{a0}' | '\u{1680}' | '\u{2000}'..='\u{200a}'
        | '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}' | '\u{3000}' | '\u{feff}')
}

/// `String.prototype.trim`.
pub fn js_trim(s: &str) -> &str {
    s.trim_matches(js_space)
}

/// The text box's value as it is stored: tabs become spaces, other control characters but the
/// newline go, at most 20 lines, no trailing white space.
pub fn clean_text(raw: &str) -> String {
    let s: String =
        raw.chars().map(|c| if c == '\t' { ' ' } else { c }).filter(|&c| !(c < ' ' && c != '\n' || c == '\u{7f}')).collect();
    let joined = s.split('\n').take(MAX_LINES).collect::<Vec<_>>().join("\n");
    joined.trim_end_matches(js_space).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Board;

    /// A host that records what input asked of it.
    struct Rec {
        board: Board,
        view: View,
        mine: Vec<String>,
        pen: Pen,
        net: Net,
        now: f64,
        ids: u32,
        log: Vec<String>,
        sent: Vec<Out>,
        text: String,
        hidden: Vec<Layer>,
        scene: Scene,
        over: Option<Overlay>,
    }

    impl Rec {
        fn new() -> Rec {
            let mut view = View::default();
            view.layout(1300.0, 800.0, 1.0);
            Rec {
                board: Board::default(),
                view,
                mine: vec![],
                pen: Pen { edit: true, me: Some(1), tool: Tool::Pen, color: Some("#111111".into()), my_color: Some("#abcdef".into()), width: 0.004, fill: false, heads: Heads::End, style: Style::default(), snap: false },
                net: Net { online: true, ready: true, local: false },
                now: 10_000.0,
                ids: 0,
                log: vec![],
                sent: vec![],
                text: String::new(),
                hidden: vec![],
                scene: Scene::default(),
                over: None,
            }
        }
        fn frames(&self) -> Vec<String> {
            self.sent.iter().map(Out::frame).collect()
        }
    }

    impl Host for Rec {
        fn board(&mut self) -> &mut Board {
            &mut self.board
        }
        fn view(&self) -> &View {
            &self.view
        }
        fn mine(&mut self) -> &mut Vec<String> {
            &mut self.mine
        }
        fn pen(&self) -> Pen {
            self.pen.clone()
        }
        fn net(&self) -> Net {
            self.net
        }
        fn hidden(&self, l: Option<Layer>) -> bool {
            l.is_some_and(|l| self.hidden.contains(&l))
        }
        fn now(&self) -> f64 {
            self.now
        }
        fn new_id(&mut self) -> String {
            self.ids += 1;
            format!("id{}", self.ids)
        }
        fn rect(&self) -> (f64, f64) {
            (0.0, 0.0)
        }
        fn bbox(&mut self, s: &Item) -> [f64; 4] {
            geom::bbox(&s.pts, s.w, s.txt.as_deref(), |l| l.chars().count() as f64 * 50.0) // px at the 100px measure font
        }
        fn send(&mut self, o: Out) {
            self.sent.push(o);
        }
        fn set_view(&mut self, z: f64, x: f64, y: f64, settle: bool) {
            self.view.set(z, x, y);
            self.log.push(format!("view {settle}"));
        }
        fn zoom_at(&mut self, f: f64, sx: f64, sy: f64, settle: bool) {
            self.view.zoom_at(f, sx, sy);
            self.log.push(format!("zoom_at {f:.4} {settle}"));
        }
        fn settle_view(&mut self) {
            self.log.push("settle".into());
        }
        fn capture(&mut self, p: i32) {
            self.log.push(format!("capture {p}"));
        }
        fn set_drawing(&mut self, on: bool) {
            self.log.push(format!("drawing {on}"));
        }
        fn request_flush(&mut self) {
            self.log.push("raf".into());
        }
        fn draw_new(&mut self, id: &str) {
            self.log.push(format!("draw_new {id}"));
        }
        fn redraw_live(&mut self) {
            self.log.push("redraw_live".into());
        }
        fn insert_stroke(&mut self, s: Item) {
            self.log.push(format!("insert {}", s.id));
            self.board.insert(s);
        }
        fn remove_stroke(&mut self, id: &str) -> bool {
            self.log.push(format!("remove {id}"));
            self.board.remove(id)
        }
        fn send_ping(&mut self, pts: Vec<f64>, touch: bool) {
            self.log.push(format!("ping {} {touch}", pts.len()));
        }
        fn ping_done(&mut self) {
            self.pen.tool = Tool::Pen;
            self.log.push("ping_done".into());
        }
        fn save_local(&mut self) {
            self.log.push("save_local".into());
        }
        fn editor_show(&mut self, ed: &Editor, text: &str) {
            self.text = text.into();
            self.log.push(format!("editor_show {} {}", ed.old.is_some(), text));
        }
        fn editor_text(&self) -> String {
            self.text.clone()
        }
        fn editor_hide(&mut self) {
            self.text.clear();
            self.log.push("editor_hide".into());
        }
        fn scene(&mut self) -> &Scene {
            let hidden = self.hidden.clone();
            self.scene = Scene::build(&self.board.strokes, |s| s.l.is_some_and(|l| hidden.contains(&l)));
            &self.scene
        }
        fn overlay(&mut self, o: Option<Overlay>) {
            self.log.push(format!("overlay {}", o.as_ref().map_or("off".into(), |o| format!("{} ghosts {} dots", o.ghosts.len(), o.dots.len()))));
            self.over = o;
        }
        fn sel_changed(&mut self, g: bool, u: bool) {
            self.log.push(format!("sel {g} {u}"));
        }
    }

    fn ptr(kind: Kind, id: i32, x: f64, y: f64) -> Ptr {
        Ptr { id, kind, button: 0, buttons: 1, pressure: 0.5, x, y, primary: true, shift: false }
    }
    fn mouse(x: f64, y: f64) -> Ptr {
        ptr(Kind::Mouse, 1, x, y)
    }

    #[test]
    fn mouse_stroke_online() {
        let (mut h, mut i) = (Rec::new(), Input::default());
        assert!(i.down(&mut h, &mouse(100.0, 100.0)));
        i.moved(&mut h, &mouse(140.0, 120.0), &[]);
        i.moved(&mut h, &mouse(180.0, 100.0), &[]); // flush still queued: no second raf
        assert_eq!(h.log, ["capture 1", "drawing true", "raf"]);
        i.flush(&mut h);
        i.moved(&mut h, &mouse(220.0, 130.0), &[]);
        i.end(&mut h, &mouse(220.0, 130.0), true);
        let f = h.frames();
        assert!(f[0].starts_with(r##"{"t":"b","id":"id1","c":"#111111","w":"##), "{}", f[0]);
        // the down point and two moves in the first frame's batch, one move in the next
        let npts = |o: &Out| if let Out::Points { pts, .. } = o { pts.len() } else { 0 };
        assert_eq!((npts(&h.sent[1]), npts(&h.sent[2])), (9, 3), "{f:?}");
        let (x, y) = h.view.board_point(100.0, 100.0);
        assert!(f[1].starts_with(&format!(r#"{{"t":"p","id":"id1","pts":[{},{},0.5,"#, r5(x), r5(y))), "{}", f[1]);
        assert!(f[3].starts_with(r#"{"t":"e","id":"id1""#), "{}", f[3]);
        assert_eq!(f.len(), 4);
        assert!(h.board.pending.contains_key("id1") && h.board.has("id1") && h.board.live.is_empty());
        assert_eq!(h.mine, ["id1"]);
        assert!(i.cur.is_none());
    }

    #[test]
    fn offline_and_loading_strokes() {
        // not ready: nothing on the wire until `ready` resends it as `s`
        let (mut h, mut i) = (Rec::new(), Input::default());
        h.net.ready = false;
        i.down(&mut h, &mouse(100.0, 100.0));
        i.moved(&mut h, &mouse(140.0, 120.0), &[]);
        i.end(&mut h, &mouse(140.0, 120.0), true);
        assert!(h.sent.is_empty());
        assert!(h.board.pending.contains_key("id1"));
        // began offline, online by the lift: goes out as `s`
        let (mut h, mut i) = (Rec::new(), Input::default());
        h.net.online = false;
        i.down(&mut h, &mouse(100.0, 100.0));
        h.net.online = true;
        i.moved(&mut h, &mouse(140.0, 120.0), &[]);
        i.end(&mut h, &mouse(140.0, 120.0), true);
        assert_eq!(h.sent.len(), 1);
        assert!(matches!(h.sent[0], Out::Stroke { txt: None, .. }));
    }

    #[test]
    fn jitter_and_pressure() {
        let (mut h, mut i) = (Rec::new(), Input::default());
        let mut p = ptr(Kind::Pen, 3, 100.0, 100.0);
        p.pressure = 0.12345;
        i.down(&mut h, &p);
        let mut q = p;
        q.x += 0.1; // under 0.0003 board units at 100%: dropped
        i.moved(&mut h, &q, &[]);
        q.x = 120.0;
        q.pressure = 0.0; // a pen at zero pressure is 0.5
        i.moved(&mut h, &q, &[q, Ptr { x: 121.0, ..q }]);
        let pts = &h.board.live["id1"].pts;
        assert_eq!(pts.len(), 9);
        assert_eq!((pts[2], pts[5], pts[8]), (0.123, 0.5, 0.5));
        assert!(i.pen_seen);
    }

    #[test]
    fn empty_stroke_is_undone() {
        let (mut h, mut i) = (Rec::new(), Input::default());
        i.down(&mut h, &mouse(100.0, 100.0));
        h.board.live.get_mut("id1").unwrap().pts.clear();
        i.end(&mut h, &mouse(100.0, 100.0), true);
        assert!(matches!(&h.sent[..], [Out::Begin { .. }, Out::Points { .. }, Out::Undo { .. }]));
        assert!(!h.board.has("id1") && h.mine.is_empty());
    }

    #[test]
    fn second_finger_turns_the_stroke_into_a_pinch() {
        let (mut h, mut i) = (Rec::new(), Input::default());
        i.down(&mut h, &ptr(Kind::Touch, 1, 100.0, 100.0));
        assert!(i.cur.is_some());
        let mut b = ptr(Kind::Touch, 2, 300.0, 100.0);
        b.primary = false;
        assert!(i.down(&mut h, &b));
        assert!(i.cur.is_none() && i.pinch.is_some());
        assert!(matches!(h.sent.last(), Some(Out::Undo { id }) if id == "id1"));
        assert!(h.board.live.is_empty());
        // spread to twice the distance: twice the zoom
        i.moved(&mut h, &Ptr { x: 500.0, ..b }, &[]);
        assert!((h.view.zoom - 2.0).abs() < 1e-9, "{}", h.view.zoom);
        // one lift ends the pinch and settles; the other finger then draws nothing
        i.end(&mut h, &b, true);
        assert!(i.pinch.is_none());
        assert_eq!(h.log.iter().filter(|l| *l == "settle").count(), 1);
        // a third finger is ignored
        let (mut h, mut i) = (Rec::new(), Input::default());
        i.down(&mut h, &ptr(Kind::Touch, 1, 0.0, 0.0));
        i.down(&mut h, &Ptr { primary: false, ..ptr(Kind::Touch, 2, 50.0, 0.0) });
        assert!(!i.down(&mut h, &Ptr { primary: false, ..ptr(Kind::Touch, 3, 90.0, 0.0) }));
    }

    #[test]
    fn a_resting_hand_is_ignored_once_a_pen_is_seen() {
        let (mut h, mut i) = (Rec::new(), Input::default());
        i.down(&mut h, &ptr(Kind::Pen, 9, 100.0, 100.0));
        // palm while the pen draws
        assert!(!i.down(&mut h, &ptr(Kind::Touch, 1, 400.0, 400.0)));
        assert!(i.touches.is_empty());
        i.end(&mut h, &ptr(Kind::Pen, 9, 100.0, 100.0), true);
        // within 400ms of the pen
        h.now += 300.0;
        assert!(!i.down(&mut h, &ptr(Kind::Touch, 1, 400.0, 400.0)));
        // later, one finger neither draws nor pans
        h.now += 1000.0;
        assert!(i.down(&mut h, &ptr(Kind::Touch, 1, 400.0, 400.0)));
        assert!(i.cur.is_none() && i.drag.is_none());
    }

    #[test]
    fn pans() {
        // a viewer's finger pans
        let (mut h, mut i) = (Rec::new(), Input::default());
        h.pen.edit = false;
        let x0 = h.view.pan_x;
        i.down(&mut h, &ptr(Kind::Touch, 1, 500.0, 300.0));
        i.moved(&mut h, &ptr(Kind::Touch, 1, 400.0, 300.0), &[]);
        assert!(h.view.pan_x > x0);
        i.end(&mut h, &ptr(Kind::Touch, 1, 400.0, 300.0), true);
        assert_eq!(h.log, ["capture 1", "view false", "settle"]);
        // Space + left button, and the middle button
        for (space, button) in [(true, 0), (false, 1)] {
            let (mut h, mut i) = (Rec::new(), Input::default());
            i.space_down = space;
            i.down(&mut h, &Ptr { button, ..mouse(10.0, 10.0) });
            assert!(i.drag.is_some() && i.cur.is_none());
        }
        // the right button does nothing
        let (mut h, mut i) = (Rec::new(), Input::default());
        assert!(i.down(&mut h, &Ptr { button: 2, ..mouse(10.0, 10.0) }));
        assert!(i.drag.is_none() && i.cur.is_none() && h.log.is_empty());
    }

    #[test]
    fn eraser_and_ping() {
        let (mut h, mut i) = (Rec::new(), Input::default());
        i.down(&mut h, &Ptr { button: 5, ..ptr(Kind::Pen, 4, 10.0, 10.0) });
        assert_eq!(h.board.live["id1"].c, ERASER_COLOR);
        assert_eq!(h.board.live["id1"].w, h.view.pen_w(0.004 * ERASER_SCALE));
        // ping: drawn here only, sent whole at the end, then back to the pen
        let (mut h, mut i) = (Rec::new(), Input::default());
        h.pen.tool = Tool::Ping;
        i.down(&mut h, &mouse(10.0, 10.0));
        assert_eq!(h.board.live["id1"].c, "#abcdef");
        i.moved(&mut h, &mouse(60.0, 60.0), &[]);
        i.end(&mut h, &mouse(60.0, 60.0), true);
        assert!(h.sent.is_empty());
        assert_eq!(h.log[h.log.len() - 3..], ["redraw_live", "ping 6 false", "ping_done"]);
        assert_eq!(h.pen.tool, Tool::Pen);
    }

    #[test]
    fn hidden_page_ends_the_stroke() {
        let (mut h, mut i) = (Rec::new(), Input::default());
        i.down(&mut h, &mouse(10.0, 10.0));
        i.moved(&mut h, &mouse(60.0, 60.0), &[]);
        i.hidden(&mut h);
        assert!(i.cur.is_none() && h.board.has("id1"));
        assert_eq!(h.log.last().unwrap(), "settle");
        // a lost capture after the lift is harmless
        i.end(&mut h, &mouse(60.0, 60.0), false);
    }

    #[test]
    fn text_new_edit_and_cancel() {
        let (mut h, mut i) = (Rec::new(), Input::default());
        h.pen.tool = Tool::Text;
        i.down(&mut h, &mouse(200.0, 200.0));
        assert_eq!(i.text_tap, Some(1));
        i.end(&mut h, &mouse(200.0, 200.0), true);
        let ed = i.editor.clone().unwrap();
        assert_eq!(ed.w, 0.004 * TEXT_SCALE);
        h.text = "hi\tthere\u{7}\n\n \u{feff}".into();
        // a tap elsewhere finishes it
        assert!(i.down(&mut h, &mouse(900.0, 600.0)));
        assert!(i.editor.is_none());
        let Some(Out::Stroke { id, txt, pts, .. }) = h.sent.last().cloned() else { panic!("{:?}", h.sent) };
        assert_eq!(txt.as_deref(), Some("hi there"));
        assert_eq!(pts, [r5(ed.x), r5(ed.y), 0.5]);
        assert_eq!(h.mine, [id.as_str()]);
        // tap on it: edit it, off the board meanwhile
        i.end(&mut h, &mouse(900.0, 600.0), true); // the finishing tap's lift places nothing
        assert!(i.editor.is_none());
        i.down(&mut h, &mouse(200.0, 200.0));
        i.end(&mut h, &mouse(200.0, 200.0), true);
        assert!(i.editor.as_ref().unwrap().old.is_some());
        assert!(!h.board.has(&id));
        assert_eq!(h.text, "hi there");
        // unchanged: put back, nothing sent
        let n = h.sent.len();
        i.close_editor(&mut h, true);
        assert!(h.board.has(&id) && h.sent.len() == n);
        // changed offline: the old one is queued as an undo, the new one waits in pending
        i.down(&mut h, &mouse(200.0, 200.0));
        i.end(&mut h, &mouse(200.0, 200.0), true);
        h.net.online = false;
        h.text = "bye".into();
        i.close_editor(&mut h, true);
        assert_eq!(h.board.undo_queue, [id.as_str()]);
        assert_eq!(h.sent.len(), n);
        assert_eq!(h.board.pending.len(), 1);
        assert_eq!(h.board.pending[0].txt.as_deref(), Some("bye"));
        // undo while typing cancels the box
        i.down(&mut h, &mouse(900.0, 100.0));
        i.end(&mut h, &mouse(900.0, 100.0), true);
        h.text = "never".into();
        i.undo(&mut h);
        assert!(i.editor.is_none());
        assert!(h.board.strokes.iter().all(|s| s.txt.as_deref() != Some("never")));
    }

    #[test]
    fn hidden_and_others_text_is_not_edited() {
        let (mut h, mut i) = (Rec::new(), Input::default());
        h.pen.tool = Tool::Text;
        let (x, y) = h.view.board_point(200.0, 200.0);
        for (id, l) in [("other", 2), ("mine", 1)] {
            h.board.insert(Item { id: id.into(), l: Some(l), q: Some(1.0), c: "#111111".into(), w: 0.02, pts: vec![x - 0.001, y - 0.001, 0.5], txt: Some("t".into()), sh: None, st: Default::default() });
        }
        h.hidden = vec![1];
        i.down(&mut h, &mouse(200.0, 200.0));
        i.end(&mut h, &mouse(200.0, 200.0), true);
        assert!(i.editor.as_ref().unwrap().old.is_none());
        i.close_editor(&mut h, false);
        h.hidden.clear();
        i.down(&mut h, &mouse(200.0, 200.0));
        i.end(&mut h, &mouse(200.0, 200.0), true);
        assert_eq!(i.editor.as_ref().unwrap().old.as_ref().unwrap().id, "mine");
    }

    #[test]
    fn a_short_text_item_opens_without_a_panic() {
        // council pass 2 G6: a text item with one number, as damaged storage can hold
        let (mut h, mut i) = (Rec::new(), Input::default());
        h.pen.tool = Tool::Text;
        let (x, _) = h.view.board_point(200.0, 200.0);
        h.board.insert(Item { id: "short".into(), l: Some(1), q: Some(1.0), c: "#111111".into(), w: 10.0, pts: vec![x - 0.001], txt: Some("t".into()), sh: None, st: Default::default() });
        i.down(&mut h, &mouse(200.0, 200.0));
        i.end(&mut h, &mouse(200.0, 200.0), true);
        let ed = i.editor.as_ref().unwrap();
        assert_eq!(ed.old.as_ref().unwrap().id, "short");
        assert_eq!((ed.x, ed.y), (x - 0.001, 0.0));
    }

    #[test]
    fn undo_skips_gone_items() {
        let (mut h, mut i) = (Rec::new(), Input::default());
        for x in [100.0, 300.0] {
            i.down(&mut h, &mouse(x, 100.0));
            i.moved(&mut h, &mouse(x + 50.0, 120.0), &[]);
            i.end(&mut h, &mouse(x + 50.0, 120.0), true);
        }
        h.board.remove("id2"); // refused, or its layer deleted
        h.sent.clear();
        i.undo(&mut h);
        assert_eq!(h.frames(), [r#"{"t":"u","id":"id1"}"#]);
        assert!(h.mine.is_empty() && !h.board.has("id1"));
        // offline queues it; a private board does not
        for local in [false, true] {
            let (mut h, mut i) = (Rec::new(), Input::default());
            h.net = Net { online: false, ready: false, local };
            i.down(&mut h, &mouse(100.0, 100.0));
            i.moved(&mut h, &mouse(150.0, 120.0), &[]);
            i.end(&mut h, &mouse(150.0, 120.0), true);
            i.undo(&mut h);
            assert_eq!(h.board.undo_queue.len(), if local { 0 } else { 1 });
            assert_eq!(h.log.last().unwrap(), "save_local");
        }
        // a viewer cannot undo
        let (mut h, mut i) = (Rec::new(), Input::default());
        h.pen.edit = false;
        h.mine.push("x".into());
        i.undo(&mut h);
        assert_eq!(h.mine.len(), 1);
    }

    #[test]
    fn keys() {
        assert_eq!(key("z", true, false, false, false, false), Key::Undo);
        assert_eq!(key("Z", false, true, false, false, false), Key::Undo);
        assert_eq!(key("z", true, false, true, false, false), Key::None);
        assert_eq!(key("z", true, false, false, false, true), Key::None);
        assert_eq!(key("Escape", false, false, false, false, true), Key::ClosePanel);
        assert_eq!(key("+", false, false, false, false, false), Key::ZoomStep(1.5));
        assert_eq!(key("=", false, false, false, false, false), Key::ZoomStep(1.5));
        assert_eq!(key("-", false, false, false, false, false), Key::ZoomStep(1.0 / 1.5));
        assert_eq!(key("0", false, false, false, false, false), Key::ZoomHome);
        assert_eq!(key("ArrowUp", false, false, false, false, false), Key::PageStep(0.0, -1.0));
        assert_eq!(key("0", true, false, false, false, false), Key::None);
        assert_eq!(key("0", false, false, false, false, true), Key::None);
        assert_eq!(key(" ", false, false, false, false, false), Key::Space);
        assert!(Key::Space.prevents() && Key::Undo.prevents() && !Key::ZoomHome.prevents());
        assert_eq!(key("g", true, false, false, false, false), Key::Group);
        assert_eq!(key("G", false, true, true, false, false), Key::Ungroup);
        assert_eq!(key("g", true, false, false, false, true), Key::None);
        assert_eq!(key("Delete", false, false, false, false, false), Key::Delete);
        assert_eq!(key("Backspace", false, false, false, false, false), Key::Delete);
        assert_eq!(key("Backspace", false, false, false, false, true), Key::None);
        assert!(Key::Group.prevents() && Key::Ungroup.prevents() && !Key::Delete.prevents());
        assert_eq!(editor_key("Enter", false, false), Some(true));
        assert_eq!(editor_key("Enter", true, false), None);
        assert_eq!(editor_key("Enter", false, true), None);
        assert_eq!(editor_key("Escape", false, false), Some(false));
        let mut i = Input::default();
        let mut h = Rec::new();
        i.key(&mut h, Key::Space);
        assert!(i.space_down);
        i.space_up();
        assert!(!i.space_down);
    }

    #[test]
    fn wheel_and_gesture() {
        let (mut h, i) = (Rec::new(), Input::default());
        i.wheel(&mut h, &Wheel { dy: 500.0, zoom_key: true, cx: 650.0, cy: 400.0, ..Wheel::default() }); // capped at 30
        assert_eq!(h.log, [format!("zoom_at {:.4} false", (-0.3f64).exp())]);
        let y0 = h.view.pan_y;
        h.view.set(2.0, h.view.pan_x, h.view.pan_y);
        let y1 = h.view.pan_y;
        i.wheel(&mut h, &Wheel { dy: 3.0, line: true, ..Wheel::default() }); // 3 lines = 48 px
        assert!((h.view.pan_y - (y1 + 48.0 / (h.view.bw * 2.0))).abs() < 1e-12, "{y0} {y1} {}", h.view.pan_y);
        // gesture: ignored while fingers are down or right after a touch
        let (mut h, mut i) = (Rec::new(), Input::default());
        i.gesture_start(&h);
        i.gesture_change(&mut h, f64::NAN, 0.0, 0.0);
        i.gesture_change(&mut h, 0.0, 0.0, 0.0);
        i.down(&mut h, &ptr(Kind::Touch, 1, 0.0, 0.0));
        i.end(&mut h, &ptr(Kind::Touch, 1, 0.0, 0.0), true);
        h.log.clear();
        i.gesture_change(&mut h, 2.0, 0.0, 0.0);
        assert!(h.log.is_empty());
        h.now += 1000.0;
        i.gesture_change(&mut h, 2.0, 0.0, 0.0);
        assert_eq!(h.log, ["zoom_at 2.0000 false"]);
    }

    #[test]
    fn text_cleaning_is_javascripts() {
        assert_eq!(clean_text("a\tb"), "a b");
        assert_eq!(clean_text("a\u{0}\u{1b}\u{7f}b\r"), "ab");
        // U+0085 is not white space to JavaScript, U+FEFF is
        assert_eq!(clean_text("x\u{85}"), "x\u{85}");
        assert_eq!(clean_text("x \u{3000}\u{feff}\n\n"), "x");
        let many = (0..30).map(|i| i.to_string()).collect::<Vec<_>>().join("\n");
        assert_eq!(clean_text(&many).lines().count(), MAX_LINES);
        assert_eq!(js_trim("\u{feff} \u{a0}"), "");
        assert_eq!(js_trim("\u{85}"), "\u{85}");
    }

    #[test]
    fn editor_box_matches_place_editor() {
        let mut v = View::default();
        v.layout(1300.0, 800.0, 1.0);
        let ed = Editor { x: 0.5, y: 0.1, c: "#111111".into(), w: 0.02, old: None };
        let (l, t, px, w) = editor_box(&ed, &v);
        let k = v.bw * v.zoom;
        assert_eq!((l, t, px), ((0.5 - v.pan_x) * k, (0.1 - v.pan_y) * k, 0.02 * k));
        assert_eq!(w, 60f64.max((2.0 - v.pan_x) * k - l - 2.0));
    }

    // ---- shapes and lines (A-166) ----

    /// The pointer at board point `(bx, by)`.
    fn at(h: &Rec, bx: f64, by: f64) -> Ptr {
        let v = &h.view;
        let k = v.bw * v.zoom;
        mouse((bx - v.pan_x) * k, (by - v.pan_y) * k)
    }

    fn drag(h: &mut Rec, i: &mut Input, from: (f64, f64), to: (f64, f64)) {
        let p = at(h, from.0, from.1);
        i.down(h, &p);
        let p = at(h, (from.0 + to.0) / 2.0, (from.1 + to.1) / 2.0);
        i.moved(h, &p, &[]);
        let p = at(h, to.0, to.1);
        i.moved(h, &p, &[]);
        i.end(h, &p, true);
    }

    fn sh_of(o: &Out) -> Option<Shape> {
        match o {
            Out::Stroke { sh, .. } => sh.clone(),
            _ => None,
        }
    }

    /// A board point well inside the visible paper.
    fn mid(h: &Rec) -> (f64, f64) {
        let v = &h.view;
        (v.pan_x + v.vis_w(v.zoom) / 2.0, v.pan_y + v.vis_h(v.zoom) / 2.0)
    }

    /// Three of my rectangles in a row, and a line from the first to the second.
    fn three_boxes_and_a_line(h: &mut Rec, i: &mut Input) -> (f64, f64) {
        let (x, y) = mid(h);
        h.pen.tool = Tool::Shape(BoxKind::Rect);
        for k in 0..3 {
            let x0 = x + 0.2 * k as f64;
            drag(h, i, (x0, y), (x0 + 0.1, y + 0.1));
        }
        h.pen.tool = Tool::Line;
        drag(h, i, (x + 0.05, y + 0.05), (x + 0.25, y + 0.05));
        assert_eq!(h.mine.len(), 4);
        h.pen.tool = Tool::Select;
        (x, y)
    }

    /// A select-tool click at board point `(x, y)`, later than any earlier one.
    fn click(h: &mut Rec, i: &mut Input, x: f64, y: f64, shift: bool) {
        h.now += 1000.0;
        let p = Ptr { shift, ..at(h, x, y) };
        i.down(h, &p);
        i.end(h, &p, true);
    }

    fn batch(o: &Out) -> Vec<Out> {
        match o {
            Out::Batch(a) => a.clone(),
            o => panic!("not one x: {}", o.frame()),
        }
    }

    #[test]
    fn a_marquee_moves_three_shapes_and_their_connector_as_one_x_and_one_undo() {
        let (mut h, mut i) = (Rec::new(), Input::default());
        let (x, y) = three_boxes_and_a_line(&mut h, &mut i);
        let line = h.mine[3].clone();
        let before: Vec<Item> = h.board.strokes.clone();
        drag(&mut h, &mut i, (x - 0.05, y - 0.05), (x + 0.55, y + 0.2));
        assert_eq!(i.sel.len(), 4, "the marquee holds all four");
        assert!(h.over.as_ref().is_some_and(|o| o.sel.is_some()), "the selection shows its box");
        let sent = h.sent.len();
        // drag the first box down: everything selected goes with it, in ONE x
        let p = at(&h, x + 0.02, y + 0.02);
        i.down(&mut h, &p);
        let p = at(&h, x + 0.02, y + 0.12);
        i.moved(&mut h, &p, &[]);
        assert_eq!(h.over.as_ref().map(|o| o.ghosts.len()), Some(4), "ghosts of three boxes and the line");
        i.end(&mut h, &p, true);
        assert_eq!(h.sent.len(), sent + 1);
        let parts = batch(&h.sent[sent]);
        assert_eq!(parts.len(), 8);
        assert!(parts[..4].iter().all(|p| matches!(p, Out::Undo { .. })) && parts[4..].iter().all(|p| matches!(p, Out::Stroke { .. })));
        for b in &before {
            assert!(!h.board.has(&b.id));
        }
        let names: Vec<Option<String>> = h.board.strokes.iter().map(|s| s.sh.as_ref().and_then(|sh| sh.node().map(String::from))).collect();
        let old_names: Vec<Option<String>> = before.iter().map(|s| s.sh.as_ref().and_then(|sh| sh.node().map(String::from))).collect();
        assert_eq!(names, old_names, "boxes keep their names, so the connector follows");
        let moved_line = h.board.strokes.iter().find(|s| s.sh.as_ref().is_some_and(Shape::is_line)).unwrap().id.clone();
        let seg = h.scene().seg(&moved_line).unwrap().clone();
        assert!((seg.p[1] - (y + 0.15)).abs() < 0.02, "the line moved down with the boxes: {:?}", seg.p);
        assert_eq!(i.sel.len(), 4, "the moved items stay selected");
        // ONE undo puts all four back where they were
        i.undo(&mut h);
        assert_eq!(h.sent.len(), sent + 2);
        assert_eq!(batch(&h.sent[sent + 1]).len(), 8);
        let pos: Vec<Vec<f64>> = h.board.strokes.iter().map(|s| s.pts.clone()).collect();
        let was: Vec<Vec<f64>> = before.iter().map(|s| s.pts.clone()).collect();
        assert_eq!(pos, was);
        assert!(i.sel.is_empty());
        // the next undo takes away the line, under the id it came back with
        assert!(!h.mine.contains(&line));
        i.undo(&mut h);
        assert_eq!(h.board.strokes.len(), 3);
        assert!(!h.board.strokes.iter().any(|s| s.sh.as_ref().is_some_and(Shape::is_line)));
    }

    #[test]
    fn groups_select_together_nest_enter_by_double_click_and_come_apart() {
        let (mut h, mut i) = (Rec::new(), Input::default());
        let (x, y) = three_boxes_and_a_line(&mut h, &mut i);
        // boxes by name: an edit gives them new ids and puts them on top
        let names: Vec<String> = h.board.strokes.iter().filter_map(|s| s.sh.as_ref().and_then(|sh| sh.node().map(String::from))).collect();
        let box_at = |h: &Rec, k: usize| h.board.strokes.iter().find(|s| s.sh.as_ref().and_then(Shape::node) == Some(names[k].as_str())).unwrap().clone();
        let (a, b, c) = ((x + 0.02, y + 0.09), (x + 0.22, y + 0.09), (x + 0.42, y + 0.09));
        click(&mut h, &mut i, a.0, a.1, false);
        assert_eq!(i.sel.len(), 1);
        click(&mut h, &mut i, b.0, b.1, true);
        assert_eq!(i.sel.len(), 2, "shift-click adds");
        assert_eq!(h.log.iter().rev().find(|l| l.starts_with("sel ")).map(String::as_str), Some("sel true false"));
        let sent = h.sent.len();
        i.key(&mut h, Key::Group);
        let parts = batch(&h.sent[sent]);
        assert_eq!(parts.len(), 4, "group = one x: two u, two s");
        let g1 = box_at(&h, 0).st.g.clone();
        assert_eq!(g1.len(), 1);
        assert_eq!(box_at(&h, 1).st.g, g1);
        assert!(box_at(&h, 2).st.g.is_empty());
        // a click on one member picks the group
        i.deselect(&mut h);
        click(&mut h, &mut i, b.0, b.1, false);
        assert_eq!(i.sel.len(), 2);
        // and the group nests in a new one with C, outer first
        click(&mut h, &mut i, c.0, c.1, true);
        assert_eq!(i.sel.len(), 3);
        i.key(&mut h, Key::Group);
        let (ga, gc) = (box_at(&h, 0).st.g.clone(), box_at(&h, 2).st.g.clone());
        assert_eq!(ga.len(), 2);
        assert_eq!(ga[1], g1[0], "the old group is inside");
        assert_eq!(gc, vec![ga[0].clone()]);
        // double-click enters a group, one level per double-click
        i.deselect(&mut h);
        click(&mut h, &mut i, a.0, a.1, false);
        assert_eq!(i.sel.len(), 3);
        let p = at(&h, a.0, a.1);
        h.now += 100.0;
        i.down(&mut h, &p);
        i.end(&mut h, &p, true);
        assert_eq!(i.group_path, vec![ga[0].clone()]);
        assert_eq!(i.sel.len(), 2, "inside the outer group, the inner one is the unit");
        click(&mut h, &mut i, a.0, a.1, false);
        assert_eq!(i.sel.len(), 2, "a single click stays at this level");
        h.now += 100.0;
        i.down(&mut h, &p);
        i.end(&mut h, &p, true);
        assert_eq!(i.group_path, ga);
        assert_eq!(i.sel, vec![box_at(&h, 0).id], "inside both, one box");
        // a click outside the entered group leaves it
        click(&mut h, &mut i, x + 0.15, y + 0.5, false);
        assert!(i.sel.is_empty() && i.group_path.is_empty());
        // ungroup takes off the outer level only
        click(&mut h, &mut i, a.0, a.1, false);
        let sent = h.sent.len();
        i.key(&mut h, Key::Ungroup);
        assert_eq!(batch(&h.sent[sent]).len(), 6);
        assert_eq!(box_at(&h, 0).st.g, g1);
        assert!(box_at(&h, 2).st.g.is_empty());
        // undo regroups
        i.undo(&mut h);
        assert_eq!(box_at(&h, 0).st.g, ga);
        // delete the group: one x, and undo brings it back
        click(&mut h, &mut i, a.0, a.1, false);
        let sent = h.sent.len();
        i.key(&mut h, Key::Delete);
        let parts = batch(&h.sent[sent]);
        assert!(parts.len() == 3 && parts.iter().all(|p| matches!(p, Out::Undo { .. })));
        assert_eq!(h.board.strokes.len(), 1, "only the line is left");
        i.undo(&mut h);
        assert_eq!(h.board.strokes.len(), 4);
        assert_eq!(box_at(&h, 0).st.g, ga);
        // Escape (deselect) empties the selection
        click(&mut h, &mut i, a.0, a.1, false);
        i.deselect(&mut h);
        assert!(i.sel.is_empty() && h.over.is_none());
    }

    #[test]
    fn select_never_picks_someone_elses_item() {
        let (mut h, mut i) = (Rec::new(), Input::default());
        let (x, y) = mid(&h);
        h.board.insert(Item { id: "theirs".into(), l: Some(2), c: "#111111".into(), w: 0.004, pts: vec![x, y, 0.5, x + 0.1, y + 0.1, 0.5], sh: Some(Shape::Box { kind: BoxKind::Rect, fill: true, n: "n9".into() }), ..Item::default() });
        h.pen.tool = Tool::Select;
        click(&mut h, &mut i, x + 0.05, y + 0.05, false);
        assert!(i.sel.is_empty());
        drag(&mut h, &mut i, (x - 0.05, y - 0.05), (x + 0.2, y + 0.2));
        assert!(i.sel.is_empty());
        i.key(&mut h, Key::Delete);
        assert!(h.sent.is_empty() && h.board.has("theirs"));
    }

    #[test]
    fn a_shape_is_dragged_out_and_sent_as_one_s() {
        let (mut h, mut i) = (Rec::new(), Input::default());
        let (x, y) = mid(&h);
        h.pen.tool = Tool::Shape(BoxKind::Ellipse);
        h.pen.fill = true;
        drag(&mut h, &mut i, (x, y), (x + 0.1, y + 0.05));
        assert_eq!(h.sent.len(), 1, "{:?}", h.frames());
        let Some(Shape::Box { kind: BoxKind::Ellipse, fill: true, n }) = sh_of(&h.sent[0]) else { panic!("{:?}", h.frames()) };
        assert!(!n.is_empty());
        assert!(h.log.iter().any(|l| l == "overlay 1 ghosts 0 dots"), "a ghost while dragging");
        assert_eq!(h.log.last().map(String::as_str), Some("save_local"));
        assert_eq!(h.over, None, "the ghost goes on release");
        assert_eq!(h.mine.len(), 1);
        // a click without a drag makes nothing
        drag(&mut h, &mut i, (x + 0.3, y), (x + 0.3, y));
        assert_eq!(h.sent.len(), 1);
    }

    #[test]
    fn the_bucket_fills_my_shape_round_trips_clears_with_the_same_colour_and_undoes() {
        let (mut h, mut i) = (Rec::new(), Input::default());
        let (x, y) = mid(&h);
        h.pen.tool = Tool::Shape(BoxKind::Rect);
        drag(&mut h, &mut i, (x, y), (x + 0.1, y + 0.1));
        let first = h.board.strokes.last().unwrap().clone();
        h.pen.tool = Tool::Bucket;
        h.pen.color = Some("#E53935".into());
        // empty board: nothing
        let n = h.sent.len();
        let p = at(&h, x + 0.3, y + 0.3);
        i.down(&mut h, &p);
        i.end(&mut h, &p, true);
        assert_eq!(h.sent.len(), n);
        // inside the shape: one x (u old + e new) with the fill on the wire
        let p = at(&h, x + 0.05, y + 0.05);
        i.down(&mut h, &p);
        i.end(&mut h, &p, true);
        let f = h.frames().last().unwrap().clone();
        assert!(f.starts_with(r#"{"t":"x""#) && f.contains(r##""fc":"#e53935""##), "{f}");
        // what the relay echoes back parses to the same fill, and draws with it
        let v: serde_json::Value = serde_json::from_str(&f).unwrap();
        let back = Item::from_value(&v["a"][1]).unwrap();
        assert_eq!(back.st.fc.as_deref(), Some("#e53935"));
        let filled = h.board.strokes.last().unwrap().clone();
        assert_eq!(filled.st.fc, back.st.fc);
        let (ops, _) = shape::item_ops(&filled, &h.scene().clone(), 1000.0).unwrap();
        assert!(ops.contains(&geom::Op::FillColor(0xe53935)), "{ops:?}");
        assert_eq!(filled.sh, first.sh, "same shape, same node");
        // the same colour again takes it off
        i.down(&mut h, &p);
        i.end(&mut h, &p, true);
        assert_eq!(h.board.strokes.last().unwrap().st.fc, None);
        // undo puts the fill back, undo again the plain shape where it was
        i.undo(&mut h);
        assert_eq!(h.board.strokes.last().unwrap().st.fc.as_deref(), Some("#e53935"));
        i.undo(&mut h);
        let b = h.board.strokes.last().unwrap();
        assert_eq!((b.st.fc.clone(), b.pts.clone()), (None, first.pts.clone()));
    }

    #[test]
    fn snap_to_grid_places_moves_and_frees_line_ends_but_anchors_win() {
        let (mut h, mut i) = (Rec::new(), Input::default());
        h.pen.snap = true;
        let s = h.view.grid_step();
        let on = |v: f64| ((v / s).round() * s - v).abs() < 1e-4;
        let (x, y) = mid(&h);
        let (x, y) = (x + 0.37 * s, y + 0.41 * s); // off the grid
        h.pen.tool = Tool::Shape(BoxKind::Rect);
        drag(&mut h, &mut i, (x, y), (x + 4.3 * s, y + 3.2 * s));
        let b = h.board.strokes.last().unwrap().clone();
        assert!([b.pts[0], b.pts[1], b.pts[3], b.pts[4]].into_iter().all(on), "placed on the grid: {:?}", b.pts);
        let Some(Shape::Box { n, .. }) = b.sh.clone() else { panic!() };
        // a line from the box (its anchor wins) to an off-grid point (snaps)
        h.pen.tool = Tool::Line;
        drag(&mut h, &mut i, (b.pts[0] + 0.5 * s, b.pts[1] + 0.33 * s), (x + 9.3 * s, y + 7.6 * s));
        let l = h.board.strokes.last().unwrap().clone();
        let Some(Shape::Line { a: Some(a), b: None, .. }) = l.sh.clone() else { panic!("{:?}", l.sh) };
        assert_eq!(a.n, n);
        assert!(on(l.pts[3]) && on(l.pts[4]), "the free end snapped: {:?}", l.pts);
        // moving the box by an off-grid amount lands its corner on the grid
        h.pen.tool = Tool::Shape(BoxKind::Rect);
        let (gx, gy) = (b.pts[0] + 0.5 * s, b.pts[1] + 0.5 * s);
        drag(&mut h, &mut i, (gx, gy), (gx + 2.37 * s, gy + 1.21 * s));
        let m = h.board.strokes.iter().find(|t| matches!(&t.sh, Some(Shape::Box { n: m, .. }) if *m == n)).unwrap().clone();
        assert!(on(m.pts[0]) && on(m.pts[1]) && (m.pts[0] - b.pts[0]).abs() > s / 2.0, "moved on the grid: {:?}", m.pts);
        // and so does a selection moved with the select tool
        h.pen.tool = Tool::Select;
        let (gx, gy) = (m.pts[0] + 0.5 * s, m.pts[1] + 0.5 * s);
        drag(&mut h, &mut i, (gx, gy), (gx - 3.61 * s, gy + 2.18 * s));
        let m2 = h.board.strokes.iter().find(|t| matches!(&t.sh, Some(Shape::Box { n: m, .. }) if *m == n)).unwrap().clone();
        assert!(on(m2.pts[0]) && on(m2.pts[1]) && m2.id != m.id, "selection moved on the grid: {:?}", m2.pts);
    }

    #[test]
    fn my_shape_moves_as_an_undo_and_a_new_item_with_the_same_node() {
        let (mut h, mut i) = (Rec::new(), Input::default());
        let (x, y) = mid(&h);
        h.pen.tool = Tool::Shape(BoxKind::Rect);
        drag(&mut h, &mut i, (x, y), (x + 0.1, y + 0.1));
        let first = h.mine[0].clone();
        let Some(Shape::Box { n, .. }) = sh_of(&h.sent[0]) else { panic!() };
        // a line from it to free space, then drag the shape (any shape tool grabs mine)
        h.pen.tool = Tool::Line;
        drag(&mut h, &mut i, (x + 0.05, y + 0.05), (x + 0.4, y + 0.05));
        let Some(Shape::Line { a: Some(a), b: None, .. }) = sh_of(&h.sent[1]) else { panic!("{:?}", h.frames()) };
        assert_eq!(a.n, n);
        h.pen.tool = Tool::Shape(BoxKind::Tri);
        let p = at(&h, x + 0.05, y + 0.05);
        i.down(&mut h, &p);
        let p = at(&h, x + 0.15, y + 0.25);
        i.moved(&mut h, &p, &[]);
        assert_eq!(h.over.as_ref().map(|o| o.ghosts.len()), Some(2), "the shape and its line");
        i.end(&mut h, &p, true);
        // one change: the old box goes and the moved one comes, applied whole by the relay
        let Out::Batch(parts) = h.sent[2].clone() else { panic!("{:?}", h.frames()) };
        assert_eq!(parts[0], Out::Undo { id: first.clone() });
        let Some(Shape::Box { kind: BoxKind::Rect, n: n2, .. }) = sh_of(&parts[1]) else { panic!("{:?}", h.frames()) };
        assert_eq!(n2, n, "same node, so the line follows");
        assert!(!h.board.has(&first));
        let line = h.mine[1].clone();
        let seg = h.scene().seg(&line).unwrap().clone();
        assert!((seg.p[1] - (y + 0.25)).abs() < 1e-4, "the attached end moved down 0.2: {:?}", seg.p);
        assert!((seg.p[2] - (x + 0.4)).abs() < 1e-4, "the free end stayed");

        // B1: undo puts the box back where it was, under its name, with the line still on it
        let Out::Stroke { id: moved, .. } = parts[1].clone() else { panic!() };
        i.undo(&mut h);
        let Out::Batch(back) = h.sent[3].clone() else { panic!("{:?}", h.frames()) };
        assert_eq!(back[0], Out::Undo { id: moved.clone() });
        let Out::Stroke { id: again, pts, sh: Some(Shape::Box { n: n3, .. }), .. } = back[1].clone() else { panic!("{back:?}") };
        assert_eq!(n3, n);
        assert_ne!(again, first, "a new id: the relay never takes one twice");
        assert!(!h.board.has(&moved) && h.board.has(&again));
        assert!((pts[1] - h.board.strokes.iter().find(|s| s.id == again).unwrap().pts[1]).abs() < 1e-9);
        let seg = h.scene().seg(&line).unwrap().clone();
        assert!((seg.p[1] - (y + 0.05)).abs() < 1e-4, "the attached end is back: {:?}", seg.p);
        // and the earlier steps still undo: the line, then the box
        i.undo(&mut h);
        assert_eq!(h.sent[4], Out::Undo { id: line.clone() });
        i.undo(&mut h);
        assert_eq!(h.sent[5], Out::Undo { id: again.clone() });
        assert!(h.board.strokes.is_empty());
        i.undo(&mut h);
        assert_eq!(h.sent.len(), 6, "nothing left");
    }

    #[test]
    fn a_refused_move_is_not_undone_twice() {
        let (mut h, mut i) = (Rec::new(), Input::default());
        let (x, y) = mid(&h);
        h.pen.tool = Tool::Shape(BoxKind::Rect);
        drag(&mut h, &mut i, (x, y), (x + 0.1, y + 0.1));
        let first = h.mine[0].clone();
        let old = Item { q: Some(1.0), ..h.board.strokes[0].clone() };
        drag(&mut h, &mut i, (x + 0.05, y + 0.05), (x + 0.15, y + 0.25));
        let Out::Batch(parts) = h.sent[1].clone() else { panic!("{:?}", h.frames()) };
        let Out::Stroke { id: moved, .. } = parts[1].clone() else { panic!() };
        // the relay refuses the change: `no` for the new box, the old one back
        h.board.refused(&moved);
        h.remove_stroke(&moved);
        h.insert_stroke(old);
        i.undo(&mut h);
        assert_eq!(h.sent.last(), Some(&Out::Undo { id: first.clone() }), "undo takes the box away, once");
        assert!(h.board.strokes.is_empty());
    }

    #[test]
    fn a_line_attaches_both_ends_and_a_dropped_end_in_space_is_free() {
        let (mut h, mut i) = (Rec::new(), Input::default());
        let (x, y) = mid(&h);
        h.pen.tool = Tool::Shape(BoxKind::Rect);
        drag(&mut h, &mut i, (x, y), (x + 0.1, y + 0.1));
        drag(&mut h, &mut i, (x + 0.3, y), (x + 0.4, y + 0.1));
        let nodes: Vec<String> = h.sent.iter().filter_map(sh_of).filter_map(|s| s.node().map(String::from)).collect();
        h.pen.tool = Tool::Line;
        h.pen.heads = Heads::Both;
        // inside each shape, nearer the facing sides
        drag(&mut h, &mut i, (x + 0.09, y + 0.05), (x + 0.31, y + 0.05));
        let Some(Shape::Line { h: Heads::Both, a: Some(a), b: Some(b) }) = sh_of(&h.sent[2]) else { panic!("{:?}", h.frames()) };
        assert_eq!((a.n.as_str(), a.s, a.t), (nodes[0].as_str(), 1, 0.5));
        assert_eq!((b.n.as_str(), b.s, b.t), (nodes[1].as_str(), 3, 0.5));
        // detached: an end dropped away from any shape keeps its drawn point
        drag(&mut h, &mut i, (x + 0.05, y + 0.09), (x + 0.05, y + 0.4));
        let Some(Shape::Line { a: Some(_), b: None, .. }) = sh_of(&h.sent[3]) else { panic!("{:?}", h.frames()) };
        // both ends on one shape is never made: the second end stays free
        drag(&mut h, &mut i, (x + 0.01, y + 0.05), (x + 0.099, y + 0.05));
        let Some(Shape::Line { a: Some(_), b: None, .. }) = sh_of(&h.sent[4]) else { panic!("{:?}", h.frames()) };
    }

    #[test]
    fn resting_on_an_anchor_expands_the_side_and_a_sub_anchor_survives_the_wire() {
        let (mut h, mut i) = (Rec::new(), Input::default());
        let (x, y) = mid(&h);
        h.pen.tool = Tool::Shape(BoxKind::Rect);
        drag(&mut h, &mut i, (x, y), (x + 0.2, y + 0.2));
        drag(&mut h, &mut i, (x + 0.5, y), (x + 0.6, y + 0.2));
        h.pen.tool = Tool::Line;
        let p = at(&h, x + 0.02, y + 0.1);
        i.down(&mut h, &p);
        // rest on the second shape's left middle
        let p = at(&h, x + 0.5, y + 0.1);
        i.moved(&mut h, &p, &[]);
        assert_eq!(h.over.as_ref().unwrap().dots.len(), 4);
        h.now += 1700.0;
        i.moved(&mut h, &p, &[]);
        // move off it, along the edge: the side expands into 15 sub-anchors
        h.now += 50.0;
        let p = at(&h, x + 0.5, y + 0.0375); // t = 0.8125 on the left side (it runs bottom to top)
        i.moved(&mut h, &p, &[]);
        assert_eq!(h.over.as_ref().unwrap().dots.len(), 3 + 15);
        i.end(&mut h, &p, true);
        let Some(Shape::Line { b: Some(b), .. }) = sh_of(&h.sent[2]) else { panic!("{:?}", h.frames()) };
        assert_eq!((b.s, b.t), (3, 0.8125));
        // the frame as another browser gets it back from the relay
        let f = h.sent[2].frame().replacen(r#""t":"s""#, r#""t":"e","q":3,"l":1"#, 1);
        let Some(crate::proto::In::End(it)) = crate::proto::In::parse(&f) else { panic!("{f}") };
        assert_eq!(it.sh, sh_of(&h.sent[2]));
        let sc = Scene::build([&h.board.strokes[0], &h.board.strokes[1], &it], |_| false);
        let seg = sc.seg(&it.id).unwrap();
        assert!((seg.p[3] - (y + 0.0375)).abs() < 1e-9, "{:?}", seg.p);
    }

}
