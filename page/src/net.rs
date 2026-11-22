//! The relay connection: app.js's network section (`connect`, `onMessage` and the handlers it
//! calls, `onError`, the paced upload in `onReady`) and the answer to `POST /api/boards` in
//! `createBoard` and `shareNow`.
//!
//! [`Conn`] holds the connection's state and makes its decisions. The socket, the timers, the
//! fetch and the DOM are the [`Host`]'s. Each socket the host opens is tagged with the number
//! [`Conn::connect`] gave it, and its events come back with that number: a socket that is no
//! longer the current one is ignored, as `ws !== sock` did. A private board's frames
//! (`localSend`) come into [`Conn::handle`] the same way the relay's do.

use crate::input::{Cur, Input};
use crate::model::Board;
use crate::proto::{In, Item, Layer, Out, ShelfEntry};
use serde_json::Value;
use std::collections::{HashSet, VecDeque};

/// The first reconnect wait, and the wait again once a connection has settled, ms.
pub const RETRY_MIN: u32 = 500;
/// The longest reconnect wait, ms.
pub const RETRY_MAX: u32 = 8000;
/// How long a socket must stay ready before the reconnect wait starts again from
/// [`RETRY_MIN`], ms. Resetting on `hello` let a link too slow for its board loop:
/// connect, take the snapshot, fall behind, be dropped, at once again.
pub const SETTLE_MS: u32 = 30_000;
/// The wait between upload bites, ms.
pub const BITE_MS: u32 = 500;

/// A timer the host runs, then hands back.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Timer {
    /// Call [`Conn::connect`] and open the socket.
    Reconnect,
    /// Call [`Conn::bite`] with this socket.
    Bite(u64),
    /// Call [`Conn::settled`] with this socket.
    Settled(u64),
}

/// What a message card offers.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Then {
    /// A link to `/`, "New board".
    NewBoard,
    /// A "Retry" button that makes a board again.
    Retry,
    /// A link back to the private board at `/`.
    Private,
}

/// The connection's view of the page, and its effects.
pub trait Host {
    /// What [`Host::paper_of_screen`] gives the crumple effect.
    type Paper;

    fn board(&mut self) -> &mut Board;
    /// `send`: to the relay, or to `localSend` on a private board.
    fn send(&mut self, o: Out);
    /// `sock.send`, on the current socket.
    fn sock_send(&mut self, frame: String);
    fn schedule(&mut self, ms: u32, t: Timer);

    /// `insertStroke`, with its drawing and `layersChanged`.
    fn insert_stroke(&mut self, s: Item, defer: bool);
    /// Items left the board: forget their boxes (`Bounds::forget`).
    fn gone(&mut self, ids: &[String]);
    /// `dropBoard` happened: forget every box.
    fn dropped(&mut self);
    fn redraw_base(&mut self, full: bool);
    fn redraw_live(&mut self);
    fn draw_new(&mut self, id: &str);
    /// The `drawing` class on the body.
    fn set_drawing(&mut self, on: bool);

    /// The status line's text.
    fn set_status(&mut self, text: String);
    /// The `ok` class on the status.
    fn set_connected(&mut self, ok: bool);
    /// The `edit` class on the body.
    fn set_edit(&mut self, on: bool);
    /// My layer's colour: the pen takes it unless another colour was picked
    /// (`if (c && (color == null || color === myColor)) color = c; myColor = c; buildSwatches()`).
    fn my_color(&mut self, c: Option<String>);
    /// `shelf = …; shelfChanged()`.
    fn set_shelf(&mut self, shelf: Vec<ShelfEntry>);
    fn fill_share(&mut self);
    fn layers_changed(&mut self);
    fn render_who(&mut self);
    fn send_label(&mut self);
    /// `layerName`: "Your", or "<name>'s".
    fn layer_name(&self, l: Layer) -> String;
    /// A toast; `undo` is the frame its "Undo" button sends.
    fn toast(&mut self, text: &str, undo: Option<Out>, ms: u32);
    /// `showMessage`.
    fn message(&mut self, text: &str, then: Then);
    /// The private board is on the relay now: write [`crate::local::Local::after_share`] to
    /// `LOCAL_KEY`, which keeps the shelf, and remove `SHARING_KEY`. `kept` are the items the
    /// relay refused (a full board or storage): they stay on the private board, never lost.
    fn forget_local(&mut self, kept: &[Item]);
    /// The board a share was going to is gone or not valid: remove `SHARING_KEY`, keep
    /// `LOCAL_KEY` as it is, and go back to the private board at `/` (council H2).
    fn share_lost(&mut self);
    /// A share was seated view-only, so nothing can be uploaded: remove `SHARING_KEY` and keep
    /// `LOCAL_KEY` as it is (council pass 3, P3-A). The page stays put to show why.
    fn share_refused(&mut self);

    /// The share is done with the private board (its last write is made, or it is lost):
    /// let another tab take the writer's lock now and not before (council G5).
    fn writer_done(&mut self);

    /// `onSheetView` after its checks: draw the preview of sheet `k`, if it is still on the shelf.
    fn sheet_view(&mut self, k: i64, a: Vec<Item>);
    /// `onPing` after its check: show it.
    fn ping(&mut self, l: Option<Layer>, k: String, pts: Vec<f64>);
    /// Pings are showing (`pings.length`).
    fn pings(&self) -> bool;

    /// `paperOfScreen`.
    fn paper_of_screen(&mut self) -> Self::Paper;
    /// The paper crumples away (`playPaper(tex, false, 950)` on the fx chain).
    fn crumple_fx(&mut self, tex: Self::Paper);
    /// `base.style.visibility = 'hidden'`.
    fn hide_base(&mut self);
    /// `old` crumples away, the board's paper (`paperOfStrokes`) unfolds, then the base is shown.
    fn unfold_fx(&mut self, old: Option<Self::Paper>);
}

/// The connection.
#[derive(Clone, Debug)]
pub struct Conn {
    /// The key in the link.
    pub key: String,
    /// A private board: only in this browser, nothing sent.
    pub local: bool,
    /// The number of the current socket (0: none yet).
    sock: u64,
    pub retry: u32,
    /// `gone` or `bad`: no more reconnects.
    pub stopped: bool,
    /// `hello` received on the current socket.
    pub online: bool,
    /// The snapshot is complete on the current socket.
    pub ready: bool,
    pub edit: bool,
    /// My layer (`None` for viewers).
    pub me: Option<Layer>,
    pub view_key: Option<String>,
    pub people: i64,
    /// The layers whose people are online (`onlineLayers`).
    pub on: Vec<Layer>,
    /// The connection part of the status (`net`).
    pub net: String,
    /// Which smoothing engine runs (`engine`).
    pub engine: String,
    /// A private board is being shared and its items are not all on the relay yet.
    pub uploading: bool,
    /// Sheet previews asked for on the current socket.
    asked: HashSet<i64>,
    /// Items still to upload, by id, and the socket the upload runs on.
    todo: VecDeque<String>,
    /// Items of the board being shared that the relay refused: they stay private.
    kept: Vec<Item>,
}

impl Default for Conn {
    fn default() -> Conn {
        Conn {
            key: String::new(),
            local: false,
            sock: 0,
            retry: RETRY_MIN,
            stopped: false,
            online: false,
            ready: false,
            edit: false,
            me: None,
            view_key: None,
            people: 0,
            on: vec![],
            net: "connecting".into(),
            engine: "js".into(),
            uploading: false,
            asked: HashSet::new(),
            todo: VecDeque::new(),
            kept: vec![],
        }
    }
}

impl Conn {
    /// The keys for the share panel's edit and view links (`fillShare`). The
    /// relay sends the view key to every edit-key holder, even one made a
    /// viewer because the layers are full; only a view-key holder gets none,
    /// and then the current key is the view key. Never the edit key as a view
    /// link.
    pub fn share_keys(&self) -> (Option<&str>, Option<&str>) {
        if self.local {
            return (None, None);
        }
        let edit = self.edit.then_some(self.key.as_str());
        let view = match (&self.view_key, self.edit) {
            (Some(v), _) => Some(v.as_str()),
            (None, false) => Some(self.key.as_str()),
            (None, true) => None,
        };
        (edit, view)
    }

    /// The status line (`showStatus`).
    pub fn status_text(&self) -> String {
        if self.local {
            return "private · only on this device".into();
        }
        let mut parts = vec![self.net.clone(), (if self.edit { "edit" } else { "view" }).into(), self.engine.clone()];
        if self.people > 0 {
            parts.push(format!("{} online", self.people));
        }
        parts.join(" · ")
    }

    /// `showStatus(text)`: an empty `text` keeps the connection part as it is.
    pub fn show_status(&mut self, h: &mut impl Host, text: &str) {
        if !text.is_empty() {
            self.net = text.into();
        }
        h.set_status(self.status_text());
    }

    /// `online && ready`: frames that need a board can be sent.
    pub fn connected(&self) -> bool {
        self.online && self.ready
    }

    /// `startLocal`, the connection part: a private board, mine to edit, on layer 1.
    pub fn start_local(&mut self) {
        self.local = true;
        self.edit = true;
        self.me = Some(1);
        self.online = false;
        self.ready = false;
    }

    /// `start(k)`: a board on the relay. The host loads the hidden layers for `k`, then opens
    /// the socket numbered by the result, if any.
    pub fn start(&mut self, h: &mut impl Host, key: &str) -> Option<u64> {
        self.key = key.into();
        self.show_status(h, "connecting");
        self.connect()
    }

    /// `connect`: the number of the socket to open now, or `None` once stopped.
    pub fn connect(&mut self) -> Option<u64> {
        if self.stopped {
            return None;
        }
        self.sock += 1;
        Some(self.sock)
    }

    /// `onopen`: the `join` frame, `who` being the contributor secret.
    pub fn join(&self, who: &str) -> Out {
        Out::Join { key: self.key.clone(), who: who.into() }
    }

    /// `onclose`.
    pub fn closed(&mut self, h: &mut impl Host, inp: &mut Input, sock: u64) {
        if sock != self.sock {
            return;
        }
        self.online = false;
        self.ready = false;
        self.people = 0;
        if let Some(cur) = inp.cur.as_mut() {
            cur.sent = false;
        }
        h.set_connected(false);
        if self.stopped {
            return;
        }
        self.show_status(h, "reconnecting");
        h.schedule(self.retry, Timer::Reconnect);
        self.retry = (self.retry * 2).min(RETRY_MAX);
    }

    /// `onmessage`: a text frame on socket `sock`.
    pub fn frame(&mut self, h: &mut impl Host, inp: &mut Input, sock: u64, text: &str) {
        if sock != self.sock {
            return;
        }
        if let Some(m) = In::parse(text) {
            self.handle(h, inp, m);
        }
    }

    /// `isMine`: the stroke I am drawing, or one of mine the relay has not confirmed.
    fn is_mine(inp: &Input, b: &mut Board, id: &str) -> bool {
        inp.cur.as_ref().is_some_and(|c| c.id == id) || b.pending.contains_key(id)
    }

    /// `onMessage`.
    pub fn handle(&mut self, h: &mut impl Host, inp: &mut Input, m: In) {
        match m {
            In::Hello { edit, me, view, layers, shelf } => self.hello(h, inp, edit, me, view, &layers, shelf),
            In::Snapshot(a) => {
                for s in a {
                    h.insert_stroke(s, true);
                }
            }
            In::Ready => self.on_ready(h),
            // After a reconnect the snapshot can replay my own unfinished stroke as the old
            // socket left it. Mine is the better copy.
            In::Begin { id, l, c, w } => {
                if !Self::is_mine(inp, h.board(), &id) {
                    h.board().begin(&id, l, &c, w);
                }
            }
            In::Points { id, pts } => {
                if !Self::is_mine(inp, h.board(), &id) {
                    if let Some(pts) = pts {
                        if h.board().extend(&id, &pts).is_some() {
                            h.draw_new(&id);
                        }
                    }
                }
            }
            In::End(s) => {
                if h.board().end_live(&s.id) {
                    h.redraw_live();
                }
                h.insert_stroke(s, false);
            }
            In::Ok { id, q } => {
                h.board().confirm(&id, q, self.uploading);
                self.upload_check(h);
            }
            In::No { id, m } => {
                if self.uploading {
                    if let Some(i) = h.board().pending.get(&id) {
                        self.kept.push(i.clone());
                    }
                }
                if h.board().refused(&id) {
                    self.removed(h, &[id]);
                }
                let text = m.map_or("Stroke not saved".into(), |m| format!("Stroke not saved: {m}"));
                h.toast(&text, None, 4000);
                self.upload_check(h);
            }
            In::Undo { id } => {
                // `cur` is the old socket's copy of the stroke I am drawing
                let cur = inp.cur.as_ref().map(|c| c.id.as_str());
                let (live, base) = h.board().undone(&id, cur);
                if live {
                    h.redraw_live();
                }
                if base {
                    self.removed(h, &[id]);
                }
                self.upload_check(h);
            }
            In::NewLayer { l, c } => {
                if h.board().new_layer(l, &c) {
                    h.layers_changed();
                    h.render_who();
                }
            }
            In::LayerDeleted { l } => self.layer_deleted(h, l),
            In::LayerRestored { l, a } => {
                if h.board().layer_restored(l, a) {
                    h.redraw_base(false);
                }
                h.layers_changed();
            }
            In::Crumpled { k, drop, shelf, .. } => self.crumpled(h, inp, k, drop, shelf),
            In::Uncrumpled { shelf, layers, a, .. } => {
                if let Some(ed) = inp.editor.as_mut() {
                    ed.old = None; // the text being edited went with the sheet
                }
                let b = h.board();
                let old_paper = b.strokes.iter().any(|s| !b.pending.contains_key(&s.id));
                let old = old_paper.then(|| h.paper_of_screen());
                let gone = h.board().clear_sheet();
                h.gone(&gone);
                h.board().uncrumpled(a, layers);
                h.set_shelf(shelf);
                // Keep the board out of sight until the paper has unfolded over it.
                h.hide_base();
                h.redraw_base(true);
                h.layers_changed();
                h.unfold_fx(old);
            }
            In::SheetView { k, a } => {
                self.asked.remove(&k);
                if let Some(a) = a {
                    h.sheet_view(k, a);
                }
            }
            In::People { n, on } => {
                self.people = n;
                self.on = on;
                self.show_status(h, "");
                h.render_who();
            }
            In::Name { l, n, c } => self.named(h, l, n, c),
            In::Ping { l, k, pts } => {
                if let Some(pts) = pts.filter(|p| p.len() >= 6) {
                    h.ping(l, k, pts);
                }
            }
            In::Error { code, m } => self.error(h, &code, m),
        }
    }

    /// Items left the board one by one (`removeStroke`).
    fn removed(&mut self, h: &mut impl Host, ids: &[String]) {
        h.gone(ids);
        h.redraw_base(false);
        h.layers_changed();
    }

    #[allow(clippy::too_many_arguments)]
    fn hello(
        &mut self,
        h: &mut impl Host,
        inp: &mut Input,
        edit: bool,
        me: Option<Layer>,
        view: Option<String>,
        layers: &[crate::proto::LayerInfo],
        shelf: Vec<ShelfEntry>,
    ) {
        self.asked.clear(); // preview requests sent on the old socket are gone
        self.edit = edit;
        self.me = if edit { me } else { None };
        self.view_key = view;
        self.online = true;
        self.ready = false;
        h.set_edit(edit);

        // Drop all board state; the snapshot follows. A stroke I am drawing right now
        // survives: it is sent whole when it ends.
        let cur = if edit { inp.cur.as_ref().map(|c| c.id.clone()) } else { None };
        let kept = h.board().hello(cur.as_deref(), self.me, layers);
        h.dropped();
        match inp.cur.as_mut() {
            Some(Cur { sent, out, .. }) if kept => {
                *sent = false;
                out.clear();
            }
            _ => {
                inp.cur = None;
                h.set_drawing(false);
            }
        }

        let mine = self.me.and_then(|l| h.board().layers.get(&l).map(|i| i.c.clone()));
        h.my_color(mine);

        h.redraw_base(true);
        h.redraw_live();
        h.set_connected(true);
        self.show_status(h, "connected");
        h.set_shelf(shelf);
        h.fill_share();
        h.layers_changed();
        h.send_label();
        h.render_who();
    }

    /// `onReady`.
    fn on_ready(&mut self, h: &mut impl Host) {
        self.ready = true;
        h.schedule(SETTLE_MS, Timer::Settled(self.sock));
        let me = if self.edit { self.me } else { None };
        // A share the relay seated view-only (a busy network, or a full board) can upload
        // nothing; `ready(None)` would drop the pending items and the next `upload_check`
        // would write an empty private board. Stop here, with the private board whole
        // (council pass 3, P3-A).
        if self.uploading && me.is_none() {
            self.stopped = true;
            self.uploading = false;
            self.kept.clear();
            h.writer_done();
            h.share_refused();
            h.message(
                "Nothing was shared: the relay could not give this share an editing seat (a busy \
                 network or a full board). Your board is still private and whole; try sharing \
                 again later.",
                Then::Private,
            );
            return;
        }
        let r = h.board().ready(me);
        if me.is_none() {
            return;
        }
        let changed = !r.todo.is_empty() || !r.undo.is_empty();
        h.redraw_base(true);
        h.gone(&r.undo);
        for id in r.undo {
            h.send(Out::Undo { id });
        }
        if changed {
            h.layers_changed();
        }
        // A board that was private until now can be large: send it in bites the relay's rate
        // limits allow (about 100 items or 120 KB every half second).
        self.todo = r.todo;
        self.bite(h, self.sock);
        self.upload_check(h); // a share of an empty board is done at once
    }

    /// The one place a share finishes: every item of the board that was private has been
    /// answered (`ok`, `no` or `u`), so drop this browser's copy of them, except the ones
    /// the relay refused: those stay on the private board, and the user is told (council H1).
    fn upload_check(&mut self, h: &mut impl Host) {
        // only a socket that may write the board has uploaded it (P3-A)
        let may_edit = self.edit && self.me.is_some();
        if self.uploading && self.ready && may_edit && h.board().pending.is_empty() {
            self.uploading = false;
            let kept = std::mem::take(&mut self.kept);
            h.forget_local(&kept);
            h.writer_done();
            if !kept.is_empty() {
                let n = kept.len();
                let s = if n == 1 { "stroke was" } else { "strokes were" };
                h.toast(&format!("{n} {s} not shared. They are kept on your private board."), None, 10000);
            }
        }
    }

    /// Socket `sock` has been ready for [`SETTLE_MS`]: if it is still the one in use and
    /// still ready, the next reconnect waits from [`RETRY_MIN`] again.
    pub fn settled(&mut self, sock: u64) {
        if sock == self.sock && self.ready {
            self.retry = RETRY_MIN;
        }
    }

    /// One `bite` of the upload, on socket `sock`. It stops when the socket changed; the next
    /// `ready` starts again.
    pub fn bite(&mut self, h: &mut impl Host, sock: u64) {
        if sock != self.sock || !self.online {
            return;
        }
        let mut todo = std::mem::take(&mut self.todo);
        for f in h.board().bite(&mut todo) {
            h.sock_send(f);
        }
        if !todo.is_empty() {
            h.schedule(BITE_MS, Timer::Bite(sock));
        }
        self.todo = todo;
    }

    fn layer_deleted(&mut self, h: &mut impl Host, l: Layer) {
        let gone = h.board().layer_deleted(l);
        if !gone.is_empty() {
            h.gone(&gone);
            h.redraw_base(false);
        }
        h.layers_changed();
        let text = format!("{} layer deleted", h.layer_name(l));
        if self.edit {
            h.toast(&text, Some(Out::LayerRestore { l }), 8000);
        } else {
            h.toast(&text, None, 4000);
        }
    }

    fn crumpled(&mut self, h: &mut impl Host, inp: &mut Input, k: i64, drop: Option<i64>, shelf: Vec<ShelfEntry>) {
        if let Some(ed) = inp.editor.as_mut() {
            ed.old = None; // the text being edited went with the sheet
        }
        let tex = h.paper_of_screen();
        let gone = h.board().clear_sheet();
        h.gone(&gone);
        for info in h.board().layers.values_mut() {
            info.del = false;
        }
        h.set_shelf(shelf);
        h.redraw_base(true);
        h.layers_changed();
        h.crumple_fx(tex);
        let text = if drop.is_some() {
            "Board crumpled. The oldest crumpled board was thrown away."
        } else {
            "Board crumpled and put on the shelf."
        };
        if self.edit {
            h.toast(text, Some(Out::Uncrumple { k }), 8000);
        } else {
            h.toast(text, None, 4000);
        }
    }

    /// `onName`.
    fn named(&mut self, h: &mut impl Host, l: Layer, n: Option<String>, c: Option<String>) {
        let Some(info) = h.board().layers.get_mut(&l) else { return };
        info.n = n;
        if let Some(c) = c.filter(|c| is_hex6(c)) {
            info.c = c;
        }
        let c = info.c.clone();
        if Some(l) == self.me {
            h.my_color(Some(c));
        }
        h.layers_changed();
        h.render_who();
        if h.pings() {
            h.redraw_live();
        }
    }

    /// `onError`.
    fn error(&mut self, h: &mut impl Host, code: &str, m: Option<String>) {
        // a share (new or resumed after a reload) whose board has expired or is not valid: the
        // private board is still whole, so go back to it; a stored key must never trap `/`
        if self.uploading && matches!(code, "gone" | "bad") {
            self.stopped = true;
            self.uploading = false;
            self.kept.clear();
            h.writer_done();
            h.share_lost();
            return;
        }
        match code {
            "gone" => {
                self.stopped = true;
                h.message("This board doesn't exist or has expired.", Then::NewBoard);
                self.show_status(h, "no such board");
            }
            "bad" => {
                self.stopped = true;
                h.message(m.as_deref().unwrap_or("This link is not valid."), Then::NewBoard);
                self.show_status(h, "bad link");
            }
            "busy" => {
                self.retry = RETRY_MAX;
                self.show_status(h, "board is busy, retrying");
            }
            "rate" => self.retry = self.retry.max(2000),
            // Too slow a link for how busy the board is: wait the longest before trying again.
            "slow" => {
                self.retry = RETRY_MAX;
                self.show_status(h, "connection too slow for this board, retrying");
            }
            // An edit link opened view-only: say so rather than leave it silent.
            "full" => h.toast(
                m.as_deref().unwrap_or("This board has no room for another contributor, so it opened view-only."),
                None,
                8000,
            ),
            _ => {}
        }
    }

    /// `renderShelf`'s request for a preview of sheet `k`: the frame to send, once per socket,
    /// and only when it can be answered.
    pub fn ask(&mut self, k: i64) -> Option<Out> {
        if self.asked.contains(&k) || !(self.local || self.connected()) {
            return None;
        }
        self.asked.insert(k);
        Some(Out::SheetView { k })
    }

    /// `shareNow` may start: a private board not already being shared, in a tab that may
    /// write it (`may_write`: it holds the lock and the board is not damaged; council H3).
    /// Sets `uploading`.
    pub fn share_begin(&mut self, may_write: bool) -> bool {
        if !self.local || self.uploading || !may_write {
            return false;
        }
        self.uploading = true;
        self.kept.clear();
        true
    }

    /// `shareNow`'s `fail`: the board stays private.
    pub fn share_failed(&mut self) {
        self.uploading = false;
    }

    /// `shareNow` once the board is made: not private any more. The host saves the private
    /// board, writes the new key to `SHARING_KEY`, then calls [`Conn::start`] with it.
    pub fn shared(&mut self) {
        self.local = false;
    }

    /// At boot on `/`, `SHARING_KEY` named a board (`sharing_target`): a share was cut short.
    /// The host loads the private board (its items are pending), moves to that board's link
    /// and calls [`Conn::start`]; `ready` sends what the relay does not have yet.
    pub fn resume_share(&mut self) {
        self.local = false;
        self.uploading = true;
        self.kept.clear();
    }
}

/// `/^#[0-9a-f]{6}$/i`.
fn is_hex6(c: &str) -> bool {
    c.len() == 7 && c.starts_with('#') && c[1..].bytes().all(|b| b.is_ascii_hexdigit())
}

/// The answer to `POST /api/boards`.
#[derive(Clone, Debug, PartialEq)]
pub enum Made {
    /// The new board's edit key.
    Board(String),
    /// The fetch failed: the server was not reached.
    Unreachable,
    /// 429: too many new boards from this address.
    Busy,
    /// Anything else.
    Failed,
}

impl Made {
    /// `status` is `None` when the fetch threw, `body` the response text when it could be read.
    pub fn of(status: Option<u16>, body: Option<&str>) -> Made {
        let Some(status) = status else { return Made::Unreachable };
        if status == 429 {
            return Made::Busy;
        }
        let edit = body
            .and_then(|b| serde_json::from_str::<Value>(b).ok())
            .and_then(|v| v.get("edit").and_then(Value::as_str).map(str::to_string));
        match edit {
            Some(edit) if (200..300).contains(&status) => Made::Board(edit),
            _ => Made::Failed,
        }
    }

    /// `createBoard`'s failure: (status, message). The message offers [`Then::Retry`].
    pub fn create_failure(&self) -> Option<(&'static str, &'static str)> {
        Some(match self {
            Made::Board(_) => return None,
            Made::Unreachable => ("offline", "Couldn't reach the server to make a board."),
            Made::Busy => ("waiting", "Too many new boards from this address. Wait a few seconds and try again."),
            Made::Failed => ("error", "The server couldn't make a board."),
        })
    }

    /// `shareNow`'s failure toast (6 s).
    pub fn share_failure(&self) -> Option<&'static str> {
        Some(match self {
            Made::Board(_) => return None,
            Made::Unreachable => "Couldn't reach the server. The board is still private.",
            Made::Busy => "Too many new boards from this address. Wait a few seconds and try again.",
            Made::Failed => "The server couldn't make a board. It is still private.",
        })
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn the_view_link_is_never_the_edit_key() {
        let mut c = Conn { key: "ek".into(), ..Conn::default() };
        // An edit-key holder made a viewer (layers full) still gets the view key.
        c.edit = false;
        c.view_key = Some("vk".into());
        assert_eq!(c.share_keys(), (None, Some("vk")));
        c.edit = true;
        assert_eq!(c.share_keys(), (Some("ek"), Some("vk")));
        // An editor without a view key shows no view link, not its own key.
        c.view_key = None;
        assert_eq!(c.share_keys(), (Some("ek"), None));
        // A view-key holder: the current key is the view key.
        c.edit = false;
        assert_eq!(c.share_keys(), (None, Some("ek")));
        c.local = true;
        assert_eq!(c.share_keys(), (None, None));
    }
    use super::*;
    use crate::input::Editor;

    #[derive(Default)]
    struct Rec {
        b: Board,
        log: Vec<String>,
        sent: Vec<String>,
        pings: bool,
    }

    impl Rec {
        fn take(&mut self) -> Vec<String> {
            std::mem::take(&mut self.log)
        }
    }

    impl Host for Rec {
        type Paper = &'static str;
        fn board(&mut self) -> &mut Board {
            &mut self.b
        }
        fn send(&mut self, o: Out) {
            self.sent.push(o.frame());
        }
        fn sock_send(&mut self, frame: String) {
            self.sent.push(frame);
        }
        fn schedule(&mut self, ms: u32, t: Timer) {
            self.log.push(format!("schedule {ms} {t:?}"));
        }
        fn insert_stroke(&mut self, s: Item, defer: bool) {
            self.log.push(format!("insert {} {defer}", s.id));
            self.b.insert(s);
        }
        fn gone(&mut self, ids: &[String]) {
            if !ids.is_empty() {
                self.log.push(format!("gone {}", ids.join(",")));
            }
        }
        fn dropped(&mut self) {
            self.log.push("dropped".into());
        }
        fn redraw_base(&mut self, full: bool) {
            self.log.push(format!("base {full}"));
        }
        fn redraw_live(&mut self) {
            self.log.push("live".into());
        }
        fn draw_new(&mut self, id: &str) {
            self.log.push(format!("draw_new {id}"));
        }
        fn set_drawing(&mut self, on: bool) {
            self.log.push(format!("drawing {on}"));
        }
        fn set_status(&mut self, text: String) {
            self.log.push(format!("status {text}"));
        }
        fn set_connected(&mut self, ok: bool) {
            self.log.push(format!("connected {ok}"));
        }
        fn set_edit(&mut self, on: bool) {
            self.log.push(format!("edit {on}"));
        }
        fn my_color(&mut self, c: Option<String>) {
            self.log.push(format!("my_color {c:?}"));
        }
        fn set_shelf(&mut self, shelf: Vec<ShelfEntry>) {
            self.log.push(format!("shelf {}", shelf.len()));
        }
        fn fill_share(&mut self) {}
        fn layers_changed(&mut self) {
            self.log.push("layers".into());
        }
        fn render_who(&mut self) {
            self.log.push("who".into());
        }
        fn send_label(&mut self) {
            self.log.push("label".into());
        }
        fn layer_name(&self, l: Layer) -> String {
            format!("L{l}'s")
        }
        fn toast(&mut self, text: &str, undo: Option<Out>, ms: u32) {
            self.log.push(format!("toast {text} {:?} {ms}", undo.map(|o| o.frame())));
        }
        fn message(&mut self, text: &str, then: Then) {
            self.log.push(format!("message {text} {then:?}"));
        }
        fn share_lost(&mut self) {
            self.log.push("share_lost".into());
        }
        fn share_refused(&mut self) {
            self.log.push("share_refused".into());
        }
        fn writer_done(&mut self) {
            self.log.push("writer_done".into());
        }
        fn forget_local(&mut self, kept: &[Item]) {
            self.log.push("forget_local".into());
            self.log.push(format!("kept {}", kept.iter().map(|i| i.id.as_str()).collect::<Vec<_>>().join(",")));
        }
        fn sheet_view(&mut self, k: i64, a: Vec<Item>) {
            self.log.push(format!("sheet {k} {}", a.len()));
        }
        fn ping(&mut self, l: Option<Layer>, k: String, pts: Vec<f64>) {
            self.log.push(format!("ping {l:?} {k} {}", pts.len()));
        }
        fn pings(&self) -> bool {
            self.pings
        }
        fn paper_of_screen(&mut self) -> &'static str {
            self.log.push("paper".into());
            "screen"
        }
        fn crumple_fx(&mut self, tex: &'static str) {
            self.log.push(format!("crumple_fx {tex}"));
        }
        fn hide_base(&mut self) {
            self.log.push("hide_base".into());
        }
        fn unfold_fx(&mut self, old: Option<&'static str>) {
            self.log.push(format!("unfold_fx {old:?}"));
        }
    }

    fn it(id: &str, l: Layer, q: Option<f64>) -> Item {
        Item { id: id.into(), l: Some(l), q, c: "#111111".into(), w: 0.004, pts: vec![0.1, 0.2, 0.5], txt: None }
    }
    fn cur(id: &str) -> Cur {
        Cur { id: id.into(), pointer: 1, out: vec![0.1, 0.2, 0.5], sent: true, pen: true, ping: false, touch: false }
    }
    const HELLO_EDIT: &str = r##"{"t":"hello","role":"edit","me":2,"view":"vk","layers":[{"l":2,"c":"#d63031"}],"shelf":[{"k":1,"n":3}]}"##;

    /// A connection on socket 1 after `hello` (and `ready` when `ready`).
    fn online(h: &mut Rec, inp: &mut Input, ready: bool) -> Conn {
        let mut c = Conn::default();
        assert_eq!(c.start(h, "KEY"), Some(1));
        c.frame(h, inp, 1, HELLO_EDIT);
        if ready {
            c.frame(h, inp, 1, r#"{"t":"ready"}"#);
        }
        h.take();
        h.sent.clear();
        c
    }

    #[test]
    fn reconnects_back_off_from_half_a_second_to_eight() {
        let (mut h, mut inp) = (Rec::default(), Input::default());
        let mut c = Conn::default();
        assert_eq!(c.start(&mut h, "KEY"), Some(1));
        assert_eq!(c.join("secret").frame(), r#"{"t":"join","key":"KEY","who":"secret"}"#);
        let mut waits = vec![];
        for sock in 1..=6 {
            c.closed(&mut h, &mut inp, sock);
            assert_eq!(c.connect(), Some(sock + 1));
            c.closed(&mut h, &mut inp, sock); // the old socket again: ignored
            waits.extend(h.take().into_iter().filter(|l| l.starts_with("schedule")));
        }
        let want: Vec<String> =
            [500, 1000, 2000, 4000, 8000, 8000].iter().map(|ms| format!("schedule {ms} Reconnect")).collect();
        assert_eq!(waits, want);
        c.frame(&mut h, &mut inp, 7, HELLO_EDIT);
        assert_eq!(c.retry, RETRY_MAX, "hello alone does not reset the wait");
        c.frame(&mut h, &mut inp, 7, r#"{"t":"ready"}"#);
        assert!(h.take().contains(&format!("schedule {SETTLE_MS} Settled(7)")));
        c.settled(6);
        assert_eq!(c.retry, RETRY_MAX, "an old socket settling changes nothing");
        c.settled(7);
        assert_eq!(c.retry, RETRY_MIN, "a socket that stayed ready resets the wait");
    }

    #[test]
    fn a_link_dropped_soon_after_ready_keeps_backing_off() {
        let (mut h, mut inp) = (Rec::default(), Input::default());
        let mut c = Conn::default();
        assert_eq!(c.start(&mut h, "KEY"), Some(1));
        let mut waits = vec![];
        for sock in 1..=5 {
            c.frame(&mut h, &mut inp, sock, HELLO_EDIT);
            c.frame(&mut h, &mut inp, sock, r#"{"t":"ready"}"#);
            // Dropped before it settled; its timer fires late, on a dead socket.
            c.closed(&mut h, &mut inp, sock);
            c.settled(sock);
            assert_eq!(c.connect(), Some(sock + 1));
            waits.extend(h.take().into_iter().filter(|l| l.ends_with("Reconnect")));
        }
        let want: Vec<String> =
            [500, 1000, 2000, 4000, 8000].iter().map(|ms| format!("schedule {ms} Reconnect")).collect();
        assert_eq!(waits, want);
        // Told it is too slow: the longest wait, and the reason shown.
        c.frame(&mut h, &mut inp, 6, HELLO_EDIT);
        c.closed(&mut h, &mut inp, 6);
        c.connect();
        c.retry = RETRY_MIN;
        c.frame(&mut h, &mut inp, 7, r#"{"t":"err","code":"slow"}"#);
        assert_eq!(c.retry, RETRY_MAX);
        assert!(h.take().iter().any(|l| l.contains("connection too slow for this board")));
    }

    #[test]
    fn a_closed_socket_drops_online_and_resends_my_stroke_whole() {
        let (mut h, mut inp) = (Rec::default(), Input::default());
        let mut c = online(&mut h, &mut inp, true);
        c.people = 3;
        inp.cur = Some(cur("mine"));
        c.closed(&mut h, &mut inp, 1);
        assert!(!c.online && !c.ready && c.people == 0);
        assert!(!inp.cur.as_ref().unwrap().sent);
        assert_eq!(h.take(), ["connected false", "status reconnecting · edit · js", "schedule 500 Reconnect"]);
    }

    #[test]
    fn frames_from_an_old_socket_are_ignored() {
        let (mut h, mut inp) = (Rec::default(), Input::default());
        let mut c = online(&mut h, &mut inp, true);
        c.closed(&mut h, &mut inp, 1);
        assert_eq!(c.connect(), Some(2));
        h.take();
        c.frame(&mut h, &mut inp, 1, r##"{"t":"e","id":"x","c":"#111111","w":0.004,"pts":[0,0,0.5]}"##);
        c.closed(&mut h, &mut inp, 1);
        c.frame(&mut h, &mut inp, 2, "not json");
        c.frame(&mut h, &mut inp, 2, r#"{"t":"nope"}"#);
        assert!(h.take().is_empty());
        assert!(!h.b.has("x"));
    }

    #[test]
    fn hello_keeps_the_stroke_i_am_drawing_only_when_i_can_edit() {
        let (mut h, mut inp) = (Rec::default(), Input::default());
        let mut c = Conn::default();
        c.start(&mut h, "KEY");
        h.b.begin("mine", Some(9), "#111111", 0.004);
        h.b.insert(it("old", 1, Some(1.0)));
        inp.cur = Some(cur("mine"));
        c.ask(1);
        h.take();
        c.frame(&mut h, &mut inp, 1, HELLO_EDIT);
        let kept = inp.cur.as_ref().unwrap();
        assert!(!kept.sent && kept.out.is_empty());
        assert_eq!(h.b.live["mine"].l, Some(2), "moved to my layer");
        assert!(!h.b.has("old"));
        assert!(c.online && !c.ready && c.edit && c.me == Some(2) && c.view_key.as_deref() == Some("vk"));
        assert_eq!(
            h.take(),
            [
                "edit true",
                "dropped",
                "my_color Some(\"#d63031\")",
                "base true",
                "live",
                "connected true",
                "status connected · edit · js",
                "shelf 1",
                "layers",
                "label",
                "who"
            ]
        );

        // a view link: no layer of mine, and the stroke is dropped
        c.frame(&mut h, &mut inp, 1, r#"{"t":"hello","role":"view","me":2}"#);
        assert!(inp.cur.is_none() && h.b.live.is_empty());
        assert_eq!(c.me, None);
        let log = h.take();
        assert!(log.contains(&"drawing false".to_string()) && log.contains(&"my_color None".to_string()));
        assert!(c.ask(1).is_none(), "not ready");
    }

    #[test]
    fn ready_uploads_in_paced_bites_on_the_same_socket_only() {
        let (mut h, mut inp) = (Rec::default(), Input::default());
        let mut c = online(&mut h, &mut inp, false);
        c.uploading = true;
        for i in 0..250 {
            let s = Item { l: None, ..it(&format!("s{i:03}"), 1, None) };
            h.b.pending.insert(s.id.clone(), s);
        }
        h.b.undo_queue.push("gone1".into());
        c.frame(&mut h, &mut inp, 1, r#"{"t":"ready"}"#);
        assert!(c.ready);
        assert_eq!(h.sent.len(), 101, "the queued undo, then the first 100 items");
        assert_eq!(h.sent[0], r#"{"t":"u","id":"gone1"}"#);
        assert!(h.sent[1].starts_with(r#"{"t":"s","id":"s000""#));
        assert_eq!(h.take(), ["schedule 30000 Settled(1)", "base true", "gone gone1", "layers", "schedule 500 Bite(1)"]);
        assert_eq!(h.b.strokes.len(), 250, "put back on the board, on my layer");
        assert!(h.b.strokes.iter().all(|s| s.l == Some(2)));

        c.bite(&mut h, 1);
        assert_eq!((h.sent.len(), h.take()), (201, vec!["schedule 500 Bite(1)".to_string()]));
        // the relay confirms one before its bite: it is skipped, and the last `ok` ends the upload
        for i in 0..249 {
            c.frame(&mut h, &mut inp, 1, &format!(r#"{{"t":"ok","id":"s{i:03}","q":{i}}}"#));
        }
        assert!(c.uploading && h.take().is_empty());
        c.bite(&mut h, 1);
        assert_eq!(h.sent.len(), 202, "only s249 was still pending");
        assert!(h.take().is_empty(), "nothing left: no more bites");
        c.frame(&mut h, &mut inp, 1, r#"{"t":"ok","id":"s249","q":249}"#);
        assert!(!c.uploading);
        assert_eq!(h.take(), ["forget_local", "kept ", "writer_done"]);

        // a bite for a socket that has gone does nothing
        h.b.pending.insert("late".into(), it("late", 2, None));
        c.todo.push_back("late".into());
        c.closed(&mut h, &mut inp, 1);
        c.connect();
        c.bite(&mut h, 1);
        assert_eq!(h.sent.len(), 202);
    }

    #[test]
    fn a_viewer_forgets_its_pending_items_at_ready() {
        let (mut h, mut inp) = (Rec::default(), Input::default());
        let mut c = Conn::default();
        c.start(&mut h, "KEY");
        c.frame(&mut h, &mut inp, 1, r#"{"t":"hello","role":"view"}"#);
        h.b.pending.insert("p".into(), it("p", 1, None));
        h.take();
        c.frame(&mut h, &mut inp, 1, r#"{"t":"ready"}"#);
        assert!(c.ready && h.b.pending.is_empty() && h.sent.is_empty());
        assert_eq!(h.take(), ["schedule 30000 Settled(1)"]);
    }

    #[test]
    fn live_frames_skip_my_own_strokes() {
        let (mut h, mut inp) = (Rec::default(), Input::default());
        let mut c = online(&mut h, &mut inp, true);
        inp.cur = Some(cur("mine"));
        h.b.begin("mine", Some(2), "#111111", 0.004);
        c.frame(&mut h, &mut inp, 1, r##"{"t":"b","id":"mine","l":5,"c":"#000000","w":0.01}"##);
        c.frame(&mut h, &mut inp, 1, r#"{"t":"p","id":"mine","pts":[1,1,0.5]}"#);
        assert_eq!((h.b.live["mine"].l, h.b.live["mine"].pts.len()), (Some(2), 0));
        c.frame(&mut h, &mut inp, 1, r##"{"t":"b","id":"r","l":5,"c":"#000000","w":0.01}"##);
        c.frame(&mut h, &mut inp, 1, r#"{"t":"p","id":"r","pts":"x"}"#);
        c.frame(&mut h, &mut inp, 1, r#"{"t":"p","id":"nobody","pts":[1,1,0.5]}"#);
        c.frame(&mut h, &mut inp, 1, r#"{"t":"p","id":"r","pts":[1,1,0.5]}"#);
        c.frame(&mut h, &mut inp, 1, r##"{"t":"e","id":"r","l":5,"q":3,"c":"#000000","w":0.01,"pts":[1,1,0.5]}"##);
        assert_eq!(h.take(), ["draw_new r", "live", "insert r false"]);
        assert!(!h.b.live.contains_key("r") && h.b.has("r"));
        c.frame(&mut h, &mut inp, 1, r#"{"t":"ss","a":[{"id":"s1","q":1},{"id":"s2","q":2}]}"#);
        assert_eq!(h.take(), ["insert s1 true", "insert s2 true"]);
    }

    #[test]
    fn a_reconnect_snapshot_does_not_replay_over_my_strokes() {
        // the old socket carried half of "drawing" and all of "sent" (pending, not yet ok'd);
        // the new socket's snapshot replays both as b+p, as the relay last saw them
        let (mut h, mut inp) = (Rec::default(), Input::default());
        let mut c = online(&mut h, &mut inp, true);
        inp.cur = Some(cur("drawing"));
        h.b.begin("drawing", Some(2), "#111111", 0.004);
        h.b.extend("drawing", &[0.1, 0.2, 0.5, 0.3, 0.4, 0.5]);
        h.b.pending.insert("sent".into(), it("sent", 2, None));
        h.b.insert(it("sent", 2, None));
        c.closed(&mut h, &mut inp, 1);
        assert_eq!(c.connect(), Some(2));
        c.frame(&mut h, &mut inp, 2, HELLO_EDIT);
        h.take();
        for id in ["drawing", "sent"] {
            c.frame(&mut h, &mut inp, 2, &format!(r##"{{"t":"b","id":"{id}","l":2,"c":"#111111","w":0.004}}"##));
            c.frame(&mut h, &mut inp, 2, &format!(r#"{{"t":"p","id":"{id}","pts":[0.1,0.2,0.5]}}"#));
        }
        assert_eq!(h.b.live["drawing"].pts.len(), 6, "my stroke is not cut back to what the relay had");
        assert!(!h.b.live.contains_key("sent"), "no ghost live stroke over my pending item");
        assert!(h.take().iter().all(|l| !l.starts_with("draw_new")), "nothing of mine is redrawn from the relay");
    }

    #[test]
    fn refused_and_undone_items_leave_the_board_and_their_boxes() {
        let (mut h, mut inp) = (Rec::default(), Input::default());
        let mut c = online(&mut h, &mut inp, true);
        h.b.insert(it("p", 2, None));
        h.b.pending.insert("p".into(), it("p", 2, None));
        c.frame(&mut h, &mut inp, 1, r#"{"t":"no","id":"p","m":"too big"}"#);
        assert_eq!(h.take(), ["gone p", "base false", "layers", "toast Stroke not saved: too big None 4000"]);
        c.frame(&mut h, &mut inp, 1, r#"{"t":"no","id":"p","m":""}"#);
        assert_eq!(h.take(), ["toast Stroke not saved None 4000"]);

        inp.cur = Some(cur("mine"));
        h.b.begin("mine", Some(2), "#111111", 0.004);
        c.frame(&mut h, &mut inp, 1, r#"{"t":"u","id":"mine"}"#);
        assert!(h.take().is_empty() && h.b.live.contains_key("mine"), "the old socket's copy of my stroke");
        h.b.insert(it("x", 3, Some(1.0)));
        h.b.begin("y", Some(3), "#111111", 0.004);
        c.frame(&mut h, &mut inp, 1, r#"{"t":"u","id":"x"}"#);
        c.frame(&mut h, &mut inp, 1, r#"{"t":"u","id":"y"}"#);
        assert_eq!(h.take(), ["gone x", "base false", "layers", "live"]);
    }

    #[test]
    fn a_deleted_layer_offers_undo_to_editors_only() {
        let (mut h, mut inp) = (Rec::default(), Input::default());
        let mut c = online(&mut h, &mut inp, true);
        h.b.insert(it("a", 3, Some(1.0)));
        h.b.insert(it("b", 3, Some(2.0)));
        c.frame(&mut h, &mut inp, 1, r#"{"t":"ld","l":3}"#);
        assert_eq!(
            h.take(),
            ["gone a,b", "base false", "layers", r#"toast L3's layer deleted Some("{\"t\":\"lr\",\"l\":3}") 8000"#]
        );
        c.edit = false;
        c.frame(&mut h, &mut inp, 1, r#"{"t":"ld","l":4}"#);
        assert_eq!(h.take(), ["layers", "toast L4's layer deleted None 4000"]);
        c.frame(&mut h, &mut inp, 1, r#"{"t":"lr","l":3,"a":[{"id":"a","q":1}]}"#);
        assert_eq!(h.take(), ["base false", "layers"]);
        c.frame(&mut h, &mut inp, 1, r#"{"t":"lr","l":4}"#);
        assert_eq!(h.take(), ["layers"]);
    }

    #[test]
    fn crumple_and_uncrumple_keep_my_unconfirmed_items() {
        let (mut h, mut inp) = (Rec::default(), Input::default());
        let mut c = online(&mut h, &mut inp, true);
        h.b.insert(it("a", 3, Some(1.0)));
        h.b.insert(it("m", 2, None));
        h.b.pending.insert("m".into(), it("m", 2, None));
        h.b.layers.get_mut(&2).unwrap().del = true;
        inp.editor = Some(Editor { x: 0.0, y: 0.0, c: "#111111".into(), w: 0.02, old: Some(it("t", 2, Some(5.0))) });
        c.frame(&mut h, &mut inp, 1, r#"{"t":"cr","k":4,"by":2,"shelf":[{"k":4,"n":1}]}"#);
        assert!(inp.editor.as_ref().unwrap().old.is_none());
        assert!(!h.b.layers[&2].del);
        assert_eq!(
            h.take(),
            [
                "paper",
                "gone a",
                "shelf 1",
                "base true",
                "layers",
                "crumple_fx screen",
                r#"toast Board crumpled and put on the shelf. Some("{\"t\":\"un\",\"k\":4}") 8000"#
            ]
        );
        c.frame(&mut h, &mut inp, 1, r#"{"t":"cr","k":5,"drop":1,"shelf":[]}"#);
        assert!(h.take().last().unwrap().contains("The oldest crumpled board was thrown away."));

        // only my unconfirmed item is left: there is no old paper to crumple away
        c.frame(&mut h, &mut inp, 1, r#"{"t":"un","k":4,"shelf":[],"a":[{"id":"z","q":2},{"id":"a","q":1}]}"#);
        assert_eq!(h.take(), ["shelf 0", "hide_base", "base true", "layers", "unfold_fx None"]);
        let ids: Vec<&str> = h.b.strokes.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, ["a", "z", "m"]);
        c.frame(&mut h, &mut inp, 1, r#"{"t":"un","k":5,"a":[]}"#);
        assert_eq!(h.take()[..2], ["paper", "gone a,z"]);
        assert_eq!(h.take(), Vec::<String>::new());
    }

    #[test]
    fn names_take_only_a_hex_colour_and_mine_moves_the_pen() {
        let (mut h, mut inp) = (Rec::default(), Input::default());
        let mut c = online(&mut h, &mut inp, true);
        h.b.new_layer(3, "#00aa00");
        c.frame(&mut h, &mut inp, 1, r#"{"t":"nm","l":3,"n":"Ann","c":"red"}"#);
        assert_eq!((h.b.layers[&3].n.as_deref(), h.b.layers[&3].c.as_str()), (Some("Ann"), "#00aa00"));
        assert_eq!(h.take(), ["layers", "who"]);
        h.pings = true;
        c.frame(&mut h, &mut inp, 1, r##"{"t":"nm","l":2,"c":"#ABCDEF"}"##);
        assert_eq!(h.b.layers[&2].n, None);
        assert_eq!(h.take(), ["my_color Some(\"#ABCDEF\")", "layers", "who", "live"]);
        c.frame(&mut h, &mut inp, 1, r#"{"t":"nm","l":9,"n":"x"}"#);
        assert!(h.take().is_empty(), "an unknown layer");
        assert!(is_hex6("#a1B2c3") && !is_hex6("#a1B2c") && !is_hex6("a1B2c3d") && !is_hex6("#ééé"));
    }

    #[test]
    fn errors_stop_or_slow_the_reconnects() {
        let (mut h, mut inp) = (Rec::default(), Input::default());
        let mut c = online(&mut h, &mut inp, true);
        c.frame(&mut h, &mut inp, 1, r#"{"t":"err","code":"rate"}"#);
        assert_eq!(c.retry, 2000);
        c.frame(&mut h, &mut inp, 1, r#"{"t":"err","code":"full","m":"no room"}"#);
        assert_eq!((c.retry, c.stopped), (2000, false), "a full board is not a reason to reconnect");
        assert!(h.take().iter().any(|e| e.contains("no room")), "the page says why it is view-only");
        c.frame(&mut h, &mut inp, 1, r#"{"t":"err","code":"busy"}"#);
        assert_eq!((c.retry, h.take()), (8000, vec!["status board is busy, retrying · edit · js".to_string()]));
        c.frame(&mut h, &mut inp, 1, r#"{"t":"err","code":"rate"}"#);
        assert_eq!(c.retry, 8000);
        c.frame(&mut h, &mut inp, 1, r#"{"t":"err","code":"what"}"#);
        assert!(h.take().is_empty());
        c.frame(&mut h, &mut inp, 1, r#"{"t":"err","code":"bad","m":"Nope."}"#);
        assert_eq!(h.take(), ["message Nope. NewBoard", "status bad link · edit · js"]);
        c.closed(&mut h, &mut inp, 1);
        assert_eq!(h.take(), ["connected false"], "stopped: no reconnect");
        assert_eq!(c.connect(), None);

        let mut c = online(&mut h, &mut inp, true);
        c.frame(&mut h, &mut inp, 1, r#"{"t":"err","code":"gone"}"#);
        assert_eq!(
            h.take(),
            ["message This board doesn't exist or has expired. NewBoard", "status no such board · edit · js"]
        );
        assert!(c.stopped);
    }

    #[test]
    fn the_status_line() {
        let mut c = Conn::default();
        assert_eq!(c.status_text(), "connecting · view · js");
        c.engine = "wasm".into();
        c.edit = true;
        c.people = 2;
        assert_eq!(c.status_text(), "connecting · edit · wasm · 2 online");
        let (mut h, mut inp) = (Rec::default(), Input::default());
        c.frame(&mut h, &mut inp, 0, r#"{"t":"n","n":3.9,"on":[1,"x",2]}"#);
        assert_eq!((c.people, c.on.clone()), (3, vec![1, 2]));
        assert_eq!(h.take(), ["status connecting · edit · wasm · 3 online", "who"]);
        c.start_local();
        assert_eq!(c.status_text(), "private · only on this device");
    }

    #[test]
    fn previews_are_asked_for_once_per_socket() {
        let (mut h, mut inp) = (Rec::default(), Input::default());
        let mut c = online(&mut h, &mut inp, true);
        assert_eq!(c.ask(1), Some(Out::SheetView { k: 1 }));
        assert_eq!(c.ask(1), None);
        c.frame(&mut h, &mut inp, 1, r#"{"t":"sv","k":1}"#);
        assert!(h.take().is_empty(), "no list: nothing drawn");
        assert_eq!(c.ask(1), Some(Out::SheetView { k: 1 }), "answered, so it can be asked again");
        c.frame(&mut h, &mut inp, 1, r#"{"t":"sv","k":1,"a":[{"id":"a"}]}"#);
        assert_eq!(h.take(), ["sheet 1 1"]);
        c.ask(2);
        c.frame(&mut h, &mut inp, 1, HELLO_EDIT);
        assert!(c.ask(2).is_none(), "not ready on the new socket");
        c.frame(&mut h, &mut inp, 1, r#"{"t":"ready"}"#);
        assert!(c.ask(2).is_some(), "hello forgot what the old socket asked");
        let mut l = Conn::default();
        l.start_local();
        assert!(l.ask(1).is_some());
    }

    #[test]
    fn pings_need_two_points() {
        let (mut h, mut inp) = (Rec::default(), Input::default());
        let mut c = online(&mut h, &mut inp, true);
        c.frame(&mut h, &mut inp, 1, r#"{"t":"pg","l":3,"k":"Ann","pts":[0,0,0.5,1,1]}"#);
        c.frame(&mut h, &mut inp, 1, r#"{"t":"pg","l":3,"pts":"x"}"#);
        assert!(h.take().is_empty());
        c.frame(&mut h, &mut inp, 1, r#"{"t":"pg","l":3,"k":"Ann","pts":[0,0,0.5,1,1,0.5]}"#);
        assert_eq!(h.take(), ["ping Some(3) A 6"]);
    }

    #[test]
    fn sharing_runs_once_and_a_failure_keeps_the_board_private() {
        let mut c = Conn::default();
        assert!(!c.share_begin(true), "not a private board");
        c.start_local();
        assert!(!c.share_begin(false) && !c.uploading, "a read-only tab or a damaged board (H3)");
        assert!(c.share_begin(true) && c.uploading);
        assert!(!c.share_begin(true), "already sharing");
        c.share_failed();
        assert!(c.share_begin(true));
        c.shared();
        assert!(!c.local && c.uploading, "uploading lasts until the last answer");
        let mut r = Conn::default();
        r.resume_share();
        assert!(!r.local && r.uploading);
    }

    /// Council H2: a share whose board has gone (expired, or a database reset) or is not
    /// valid goes back to the private board and clears the stored key. On 5884277 it showed
    /// "doesn't exist" with a New board link to `/`, which resumed the same share: a loop.
    #[test]
    fn a_share_to_a_gone_board_goes_back_to_the_private_board() {
        for code in ["gone", "bad"] {
            let (mut h, mut inp) = (Rec::default(), Input::default());
            let mut c = Conn::default();
            c.resume_share();
            c.start(&mut h, "KEY");
            h.take();
            c.frame(&mut h, &mut inp, c.sock, &format!(r#"{{"t":"err","code":"{code}"}}"#));
            let log = h.take();
            assert_eq!(log, ["writer_done", "share_lost"], "{code}");
            assert!(!c.uploading && c.stopped, "{code}");
        }
        // a plain link to a gone board still says so
        let (mut h, mut inp) = (Rec::default(), Input::default());
        let mut c = Conn::default();
        c.start(&mut h, "KEY");
        h.take();
        c.frame(&mut h, &mut inp, c.sock, r#"{"t":"err","code":"gone"}"#);
        assert!(h.take().iter().any(|l| l.starts_with("message This board doesn't exist")));
    }

    /// Council H1: a refused item of a share is kept on the private board, never dropped, and
    /// the user is told. On 5884277 the share finished with an empty private board.
    #[test]
    fn a_share_keeps_refused_items_on_the_private_board() {
        let (mut h, mut inp) = (Rec::default(), Input::default());
        let mut c = online(&mut h, &mut inp, false);
        c.local = true;
        assert!(c.share_begin(true));
        c.shared();
        for id in ["a", "b", "c"] {
            h.b.pending.insert(id.into(), it(id, 2, None));
            h.b.insert(it(id, 2, None));
        }
        c.frame(&mut h, &mut inp, 1, r#"{"t":"ready"}"#);
        c.frame(&mut h, &mut inp, 1, r#"{"t":"ok","id":"a","q":1}"#);
        c.frame(&mut h, &mut inp, 1, r#"{"t":"no","id":"b","m":"board full"}"#);
        h.take();
        c.frame(&mut h, &mut inp, 1, r#"{"t":"no","id":"c","m":"board full"}"#);
        let log = h.take();
        assert!(log.contains(&"forget_local".to_string()), "{log:?}");
        assert!(log.contains(&"kept b,c".to_string()), "b and c stay private: {log:?}");
        assert!(log.iter().any(|l| l.starts_with("toast 2 strokes were not shared")), "{log:?}");

        // the next share starts with nothing kept
        let (mut h, mut inp) = (Rec::default(), Input::default());
        c = online(&mut h, &mut inp, false);
        c.kept.push(it("old", 2, None));
        c.local = true;
        assert!(c.share_begin(true));
        c.shared();
        c.frame(&mut h, &mut inp, 1, r#"{"t":"ready"}"#);
        let log = h.take();
        assert!(log.contains(&"kept ".to_string()) && !log.iter().any(|l| l.starts_with("toast")), "{log:?}");
    }

    #[test]
    fn a_share_finishes_on_the_last_answer_whatever_it_is() {
        let last = [r#"{"t":"ok","id":"b","q":2}"#, r#"{"t":"no","id":"b","m":"too big"}"#, r#"{"t":"u","id":"b"}"#];
        for answer in last {
            let (mut h, mut inp) = (Rec::default(), Input::default());
            let mut c = online(&mut h, &mut inp, false);
            c.uploading = true;
            for id in ["a", "b"] {
                h.b.pending.insert(id.into(), it(id, 2, None));
                h.b.insert(it(id, 2, None));
            }
            c.frame(&mut h, &mut inp, 1, r#"{"t":"ready"}"#);
            c.frame(&mut h, &mut inp, 1, r#"{"t":"ok","id":"a","q":1}"#);
            assert!(c.uploading && !h.take().contains(&"forget_local".to_string()), "b is still out");
            c.frame(&mut h, &mut inp, 1, answer);
            assert!(!c.uploading, "{answer} ends it");
            assert_eq!(h.take().iter().filter(|l| *l == "forget_local").count(), 1, "{answer}");
            c.frame(&mut h, &mut inp, 1, r#"{"t":"ok","id":"b","q":2}"#);
            assert!(!h.take().contains(&"forget_local".to_string()), "a late answer does not finish it twice");
        }

        // an empty private board is shared as soon as the new board is ready
        let (mut h, mut inp) = (Rec::default(), Input::default());
        let mut c = online(&mut h, &mut inp, false);
        c.uploading = true;
        c.frame(&mut h, &mut inp, 1, r#"{"t":"u","id":"someone"}"#);
        assert!(c.uploading, "nothing finishes before ready");
        c.frame(&mut h, &mut inp, 1, r#"{"t":"ready"}"#);
        assert!(!c.uploading);
        assert!(h.take().contains(&"forget_local".to_string()));
    }

    #[test]
    fn a_share_seated_view_only_keeps_the_private_board() {
        // council pass 3 P3-A: the relay seats the share view-only (the G7 bucket, or a full
        // board); ready(None) dropped the pending items and the next `u` or a reconnect seated
        // as editor finished the share with nothing uploaded, writing an empty private board
        let (mut h, mut inp) = (Rec::default(), Input::default());
        let mut c = Conn::default();
        assert_eq!(c.start(&mut h, "KEY"), Some(1));
        c.uploading = true;
        for id in ["a", "b"] {
            h.b.pending.insert(id.into(), it(id, 2, None));
            h.b.insert(it(id, 2, None));
        }
        c.frame(&mut h, &mut inp, 1, r#"{"t":"hello","role":"view"}"#);
        c.frame(&mut h, &mut inp, 1, r#"{"t":"ready"}"#);
        c.frame(&mut h, &mut inp, 1, r#"{"t":"u","id":"someone"}"#);
        c.frame(&mut h, &mut inp, 1, HELLO_EDIT);
        c.frame(&mut h, &mut inp, 1, r#"{"t":"ready"}"#);
        let log = h.take();
        assert!(!log.iter().any(|l| l == "forget_local"), "{log:?}");
        assert_eq!(log.iter().filter(|l| *l == "writer_done").count(), 1, "{log:?}");
        assert!(log.iter().any(|l| l == "share_refused"), "{log:?}");
        assert!(log.iter().any(|l| l.starts_with("message Nothing was shared") && l.ends_with("Private")), "{log:?}");
        assert!(!c.uploading && c.stopped, "the share is over and does not reconnect");
        assert!(c.connect().is_none(), "stopped: no reconnect");

        // and a share still marked uploading never finishes on a socket that cannot write
        let (mut h, mut inp) = (Rec::default(), Input::default());
        let mut c = Conn::default();
        c.start(&mut h, "KEY");
        c.frame(&mut h, &mut inp, 1, r#"{"t":"hello","role":"view"}"#);
        c.frame(&mut h, &mut inp, 1, r#"{"t":"ready"}"#);
        c.uploading = true;
        c.frame(&mut h, &mut inp, 1, r#"{"t":"u","id":"someone"}"#);
        assert!(!h.take().iter().any(|l| l == "forget_local"), "upload_check needs edit and me");
    }

    #[test]
    fn the_writer_lock_is_held_until_the_share_is_done_with_the_private_board() {
        // council pass 2 G5: let go at the start, a waiting tab writes the private board
        // while the share still has its last write to make
        let (mut h, mut inp) = (Rec::default(), Input::default());
        let mut c = online(&mut h, &mut inp, false);
        c.uploading = true;
        h.b.pending.insert("a".into(), it("a", 2, None));
        h.b.insert(it("a", 2, None));
        c.frame(&mut h, &mut inp, 1, r#"{"t":"ready"}"#);
        assert!(!h.take().contains(&"writer_done".to_string()), "held while a is still out");
        c.frame(&mut h, &mut inp, 1, r#"{"t":"ok","id":"a","q":1}"#);
        let log = h.take();
        let at = |w: &str| log.iter().position(|l| l == w);
        assert!(at("forget_local").is_some() && at("writer_done") > at("forget_local"), "{log:?}");

        // a share whose board has gone lets it go too, on its way back to `/`
        let (mut h, mut inp) = (Rec::default(), Input::default());
        let mut c = online(&mut h, &mut inp, false);
        c.uploading = true;
        c.frame(&mut h, &mut inp, 1, r#"{"t":"err","code":"gone"}"#);
        assert_eq!(h.take().iter().filter(|l| *l == "writer_done").count(), 1);
    }

    #[test]
    fn the_answer_to_making_a_board() {
        assert_eq!(Made::of(None, None), Made::Unreachable);
        assert_eq!(Made::of(Some(429), Some(r#"{"edit":"E"}"#)), Made::Busy);
        assert_eq!(Made::of(Some(200), Some(r#"{"edit":"E","view":"V"}"#)), Made::Board("E".into()));
        assert_eq!(Made::of(Some(201), Some(r#"{"edit":"E"}"#)), Made::Board("E".into()));
        assert_eq!(Made::of(Some(500), Some(r#"{"edit":"E"}"#)), Made::Failed);
        assert_eq!(Made::of(Some(200), Some(r#"{"edit":5}"#)), Made::Failed);
        assert_eq!(Made::of(Some(200), Some("<html>")), Made::Failed);
        assert_eq!(Made::of(Some(200), None), Made::Failed);
        assert_eq!(Made::Busy.create_failure().unwrap().0, "waiting");
        assert_eq!(Made::Unreachable.create_failure().unwrap().0, "offline");
        assert_eq!(Made::Failed.share_failure(), Some("The server couldn't make a board. It is still private."));
        assert!(Made::Board("E".into()).create_failure().is_none() && Made::Board("E".into()).share_failure().is_none());
    }
}
