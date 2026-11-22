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
use crate::proto::{Item, Layer, Out};
use crate::view::{clamp, js_round, View, MAX_ZOOM, WH, WY};
use indexmap::IndexMap;

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
        if t == Kind::Touch && self.pen_seen {
            return true;
        }
        h.capture(e.id);
        if pen.tool == Tool::Text {
            self.text_tap = Some(e.id);
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
        let f = Item { id: id.clone(), l: h.pen().me, q: None, c: s.c, w: s.w, pts, txt: None };
        h.insert_stroke(f.clone());
        h.redraw_live();
        h.board().pending.insert(id.clone(), f.clone());
        h.mine().push(id.clone());
        // Confirmed by `ok`. If the socket is down or still loading the snapshot, the stroke
        // goes out as `s` when `ready` arrives.
        if sent {
            h.send(Out::End { id, c: f.c, w: f.w, pts: f.pts });
        } else if online_ready(h) {
            h.send(Out::Stroke { id, c: f.c, w: f.w, pts: f.pts, txt: None });
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
        let f = Item { id: id.clone(), l: me, q: None, c: ed.c, w: r5(ed.w), pts: vec![r5(ed.x), r5(ed.y), 0.5], txt: Some(txt) };
        h.insert_stroke(f.clone());
        h.board().pending.insert(id.clone(), f.clone());
        h.mine().push(id.clone());
        if online_ready(h) {
            h.send(Out::Stroke { id, c: f.c, w: f.w, pts: f.pts, txt: f.txt });
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
                let (ax, ay) = (s.pts[0], s.pts[1]);
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
        while let Some(id) = h.mine().pop() {
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
            _ => {}
        }
    }

    /// `keyup` of Space, or the window losing focus.
    pub fn space_up(&mut self) {
        self.space_down = false;
    }
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
}

impl Key {
    /// Whether the handler calls `preventDefault`.
    pub fn prevents(self) -> bool {
        matches!(self, Key::Undo | Key::Space)
    }
}

/// The window `keydown` handler's choice. `in_input`: the target is an `<input>`.
pub fn key(key: &str, ctrl: bool, meta: bool, shift: bool, alt: bool, in_input: bool) -> Key {
    if (ctrl || meta) && !shift && !alt && key.to_lowercase() == "z" {
        return if in_input { Key::None } else { Key::Undo };
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
    }

    impl Rec {
        fn new() -> Rec {
            let mut view = View::default();
            view.layout(1300.0, 800.0, 1.0);
            Rec {
                board: Board::default(),
                view,
                mine: vec![],
                pen: Pen { edit: true, me: Some(1), tool: Tool::Pen, color: Some("#111111".into()), my_color: Some("#abcdef".into()), width: 0.004 },
                net: Net { online: true, ready: true, local: false },
                now: 10_000.0,
                ids: 0,
                log: vec![],
                sent: vec![],
                text: String::new(),
                hidden: vec![],
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
    }

    fn ptr(kind: Kind, id: i32, x: f64, y: f64) -> Ptr {
        Ptr { id, kind, button: 0, buttons: 1, pressure: 0.5, x, y, primary: true }
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
            h.board.insert(Item { id: id.into(), l: Some(l), q: Some(1.0), c: "#111111".into(), w: 0.02, pts: vec![x - 0.001, y - 0.001, 0.5], txt: Some("t".into()) });
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
}
