//! The browser glue: app.js's DOM, socket, storage, timer and event code, wired to the page
//! modules. Everything that decides lives in those modules; this file only moves values
//! between them and the browser.
//!
//! One [`App`] holds the connection, the input state and the [`Page`] (the host both of them
//! drive). Every browser callback borrows it with `try_borrow_mut` and drops the event when it
//! is already borrowed: that only happens when the browser runs a handler synchronously inside
//! another one (the text box's `blur` while `close_editor` hides it, which has nothing left to
//! do by then). The name and colour `change` events, which a panel closing inside another
//! handler fires the same way, are not dropped: [`with_soon`] runs them once it returns. While `conn.handle` runs the page cannot read `conn`, so the page keeps copies
//! of the fields it needs ([`App::sync`]) and the panels that read `conn` are drawn after the
//! dispatch ([`App::after`]). A private board's frames go to an outbox that
//! [`App::pump`] answers once the dispatch is over, so `conn.handle` never recurses.
//!
//! Names and other text from people only ever reach the page through `textContent`, inside a
//! `<bdi>` where they sit in a sentence. Positions are set one style property at a time: a
//! strict `style-src` refuses `cssText`.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use indexmap::IndexSet;
use js_sys::{Array, Function, Promise, Reflect};
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::{
    AddEventListenerOptions, Blob, BlobPropertyBag, CanvasRenderingContext2d, Document, Element, Event,
    EventTarget, HtmlAnchorElement, HtmlButtonElement, HtmlCanvasElement, HtmlElement, HtmlInputElement,
    HtmlTextAreaElement, KeyboardEvent, MessageEvent, PointerEvent, RequestInit, Response, Storage,
    StorageEvent, Url, WebSocket, WheelEvent, Window,
};

use crate::fx::{Fx, Paper};
use crate::geom::{self, FIXED_COLORS, FONT, LINE_HEIGHT, PING_COLOR, PING_MS};
use crate::input::{self, Editor, Input, Kind, Key, Net, Overlay, Pen, Ptr, Tool, Wheel};
use crate::local::{self, Local, Route, Save, LOCAL_KEY, SHARING_KEY};
use crate::model::Board;
use crate::names;
use crate::net::{self, Conn, Made, Then, Timer};
use crate::proto::{In, Item, Layer, LayerInfo, Out, ShelfEntry};
use crate::render::{self, Bounds, Redraw, Step, BUDGET_MS};
use crate::ask::{ask_key, Answer, AskKey, ShareGate};
use crate::shape::{BoxKind, Dash, Heads, Scene, Shape, Style};
use crate::view::{Grid, View, WW, WX, WY};

/// How long a ping from someone else lights the people button.
const NUDGE_MS: i32 = 20_000;
/// How often a private board is written to storage.
const SAVE_MS: i32 = 1500;
/// The Web Lock that makes one tab the private board's writer.
const LOCK: &str = "wb.local";
/// A sheet preview's size.
const PV_W: u32 = 256;
const PV_H: u32 = 144;

thread_local! {
    static APP: RefCell<Option<Rc<RefCell<App>>>> = const { RefCell::new(None) };
}

/// Run `f` on the app, then [`App::after`]. `None` (the event is dropped) when the app is
/// busy: a handler the browser ran inside another one.
fn with<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    let app = APP.with(|a| a.borrow().clone())?;
    let mut a = app.try_borrow_mut().ok()?;
    a.sync();
    let r = f(&mut a);
    a.after();
    Some(r)
}

/// [`with`] for an event that must not be lost, such as a `change` that the browser fires
/// inside another handler when that handler hides a panel and blurs its input. When the app
/// is busy it runs once the current handler has returned.
fn with_soon(f: impl FnOnce(&mut App) + 'static) {
    let busy = APP.with(|a| a.borrow().as_ref().is_some_and(|app| app.try_borrow_mut().is_err()));
    if busy {
        timeout(0, move || {
            with(f);
        });
    } else {
        with(f);
    }
}

// ---- small browser helpers ----------------------------------------------------

fn win() -> Window {
    web_sys::window().expect("a window")
}

fn doc() -> Document {
    win().document().expect("a document")
}

fn by_id<T: JsCast>(id: &str) -> Option<T> {
    doc().get_element_by_id(id)?.dyn_into::<T>().ok()
}

fn html(id: &str) -> Option<HtmlElement> {
    by_id::<HtmlElement>(id)
}

fn set_hidden(id: &str, hidden: bool) {
    if let Some(e) = html(id) {
        e.set_hidden(hidden);
    }
}

fn is_hidden(id: &str) -> bool {
    html(id).is_none_or(|e| e.hidden())
}

fn set_text(id: &str, text: &str) {
    if let Some(e) = doc().get_element_by_id(id) {
        e.set_text_content(Some(text));
    }
}

fn css(e: &HtmlElement, prop: &str, value: &str) {
    let _ = e.style().set_property(prop, value);
}

/// The hand's cursor: grab while Space is held, grabbing while a pan drag is live.
fn mark_pan(inp: &Input, page: &Page) {
    toggle(&page.d.body, "space", inp.space_down);
    toggle(&page.d.body, "panning", inp.drag.is_some());
}

fn toggle(e: &Element, class: &str, on: bool) {
    let _ = e.class_list().toggle_with_force(class, on);
}

fn attr(e: &Element, name: &str, value: &str) {
    let _ = e.set_attribute(name, value);
}

/// `el(tag, cls, text)`.
fn el(tag: &str, class: Option<&str>, text: Option<&str>) -> HtmlElement {
    let e: HtmlElement = doc().create_element(tag).expect("an element").unchecked_into();
    if let Some(c) = class {
        e.set_class_name(c);
    }
    if let Some(t) = text {
        e.set_text_content(Some(t));
    }
    e
}

/// A name, isolated so a right-to-left name cannot reorder the text round it.
fn bdi(name: &str) -> HtmlElement {
    el("bdi", None, Some(name))
}

fn button(text: &str) -> HtmlElement {
    let b = el("button", None, Some(text));
    attr(&b, "type", "button");
    b
}

fn clear(e: &Element) {
    e.set_text_content(None);
}

fn now() -> f64 {
    win().performance().map_or(0.0, |p| p.now())
}

fn random() -> f64 {
    js_sys::Math::random()
}

/// A number field of a JavaScript object (`clientX` is fractional, unlike web-sys's `i32`).
fn num(o: &JsValue, k: &str) -> f64 {
    Reflect::get(o, &JsValue::from_str(k)).ok().and_then(|v| v.as_f64()).unwrap_or(0.0)
}

/// `Number(s)`.
fn js_number(s: &str) -> f64 {
    JsValue::from_str(s).unchecked_into_f64()
}

fn timeout(ms: i32, f: impl FnOnce() + 'static) -> i32 {
    let cb = Closure::once_into_js(f);
    win().set_timeout_with_callback_and_timeout_and_arguments_0(cb.unchecked_ref(), ms).unwrap_or(0)
}

fn clear_timeout(id: i32) {
    if id != 0 {
        win().clear_timeout_with_handle(id);
    }
}

fn raf(f: impl FnOnce() + 'static) {
    let cb = Closure::once_into_js(f);
    let _ = win().request_animation_frame(cb.unchecked_ref());
}

/// `p.then(ok, err)`. The callback that never runs is leaked with its closure.
fn then(p: &JsValue, ok: impl FnOnce(JsValue) + 'static, err: impl FnOnce(JsValue) + 'static) {
    let Ok(t) = Reflect::get(p, &JsValue::from_str("then")) else { return };
    let Some(t) = t.dyn_ref::<Function>() else { return };
    let _ = t.call2(p, &Closure::once_into_js(ok), &Closure::once_into_js(err));
}

fn listen(t: &EventTarget, ev: &str, f: impl FnMut(Event) + 'static) {
    let c = Closure::<dyn FnMut(Event)>::new(f);
    let _ = t.add_event_listener_with_callback(ev, c.as_ref().unchecked_ref());
    c.forget();
}

/// A listener that may call `preventDefault` on a scroll-like event.
fn listen_active(t: &EventTarget, ev: &str, f: impl FnMut(Event) + 'static) {
    let c = Closure::<dyn FnMut(Event)>::new(f);
    let o = AddEventListenerOptions::new();
    o.set_passive(false);
    let _ = t.add_event_listener_with_callback_and_add_event_listener_options(ev, c.as_ref().unchecked_ref(), &o);
    c.forget();
}

fn listen_id(id: &str, ev: &str, f: impl FnMut(Event) + 'static) {
    if let Some(e) = doc().get_element_by_id(id) {
        listen(&e, ev, f);
    }
}

/// The closest element to the event's target matching `sel`.
fn closest(e: &Event, sel: &str) -> Option<Element> {
    e.target()?.dyn_into::<Element>().ok()?.closest(sel).ok()?
}

fn ctx2d(c: &HtmlCanvasElement) -> Option<CanvasRenderingContext2d> {
    c.get_context("2d").ok()??.dyn_into().ok()
}

fn canvas() -> HtmlCanvasElement {
    doc().create_element("canvas").expect("a canvas").unchecked_into()
}

fn storage() -> Option<Storage> {
    win().local_storage().ok().flatten()
}

/// `recall`: localStorage only.
fn recall(k: &str) -> Option<String> {
    storage()?.get_item(k).ok().flatten()
}

/// `remember`: localStorage only; a full or blocked storage is ignored.
fn remember(k: &str, v: &str) {
    if let Some(s) = storage() {
        let _ = s.set_item(k, v);
    }
}

fn origin() -> String {
    win().location().origin().unwrap_or_default()
}

fn replace_path(path: &str) {
    if let Ok(h) = win().history() {
        let _ = h.replace_state_with_url(&JsValue::NULL, "", Some(path));
    }
}

/// `random().toString(36).slice(2, 12) + Date.now().toString(36)`.
fn new_id() -> String {
    let r: String = js_sys::Number::from(random()).to_string(36).map(String::from).unwrap_or_default();
    let t: String = js_sys::Number::from(js_sys::Date::now()).to_string(36).map(String::from).unwrap_or_default();
    let head: String = r.chars().skip(2).take(10).collect();
    head + &t
}

fn ptr_of(e: &PointerEvent) -> Ptr {
    Ptr {
        id: e.pointer_id(),
        kind: match e.pointer_type().as_str() {
            "pen" => Kind::Pen,
            "touch" => Kind::Touch,
            _ => Kind::Mouse,
        },
        button: e.button(),
        buttons: e.buttons(),
        pressure: e.pressure() as f64,
        x: num(e, "clientX"),
        y: num(e, "clientY"),
        primary: e.is_primary(),
        shift: e.shift_key(),
    }
}

/// `e.getCoalescedEvents()`, where the browser has it.
fn coalesced(e: &PointerEvent) -> Vec<Ptr> {
    let Ok(f) = Reflect::get(e, &JsValue::from_str("getCoalescedEvents")) else { return vec![] };
    let Some(f) = f.dyn_ref::<Function>() else { return vec![] };
    let Ok(a) = f.call0(e) else { return vec![] };
    let Ok(a) = a.dyn_into::<Array>() else { return vec![] };
    a.iter().filter_map(|v| v.dyn_into::<PointerEvent>().ok()).map(|p| ptr_of(&p)).collect()
}

// ---- the page -----------------------------------------------------------------

/// A ping on screen. Shared between `pings`, `lastPing` and the nudge, as app.js shared the
/// object: a jump restarts its glow everywhere.
struct Ping {
    l: Option<Layer>,
    k: String,
    pts: Vec<f64>,
    t0: Cell<f64>,
    bb: [f64; 4],
}

/// The elements the page keeps at hand.
struct Dom {
    body: HtmlElement,
    base: HtmlCanvasElement,
    live: HtmlCanvasElement,
    spare: HtmlCanvasElement,
    bctx: CanvasRenderingContext2d,
    lctx: CanvasRenderingContext2d,
    sctx: CanvasRenderingContext2d,
    board: HtmlElement,
    paper: HtmlElement,
    ta: HtmlTextAreaElement,
    toast: HtmlElement,
    msg: HtmlElement,
    status: HtmlElement,
    who: HtmlElement,
    /// The socket handlers, made once; each socket carries its number as `wbSock`.
    on_open: Function,
    on_message: Function,
    on_close: Function,
}

/// The host: the board, the view and everything on screen.
pub struct Page {
    board: Board,
    view: View,
    redraw: Redraw,
    bounds: Bounds,
    local: Local,
    mine: Vec<String>,
    hidden: IndexSet<Layer>,
    /// Where the shapes are, rebuilt from the board when `scene_dirty`.
    scene: Scene,
    scene_dirty: bool,
    /// The shape tool's ghost and anchor dots, drawn on the live canvas.
    over: Option<Overlay>,

    // copies of the connection's fields ([`App::sync`])
    edit: bool,
    me: Option<Layer>,
    is_local: bool,
    online: bool,
    ready: bool,
    key: String,
    people: i64,
    on: Vec<Layer>,

    tool: Tool,
    color: Option<String>,
    my_color: Option<String>,
    width: f64,
    fill: bool,
    heads: Heads,
    grid: Grid,
    /// The style new items get (line style, fill colour, bold/italic).
    style: Style,
    /// The shape menu's last pick, which the shape button shows and returns to.
    shape_pick: Tool,
    /// What the Group button does now (from the selection).
    can_group: bool,
    can_ungroup: bool,
    /// The share question; nothing reaches the relay until it is answered Share.
    ask: ShareGate,
    my_name: String,
    my_hue: Option<f64>,

    /// A private board's frames, for [`App::pump`].
    outbox: Vec<Out>,
    // panels drawn by [`App::after`]
    who_dirty: bool,
    share_dirty: bool,
    shelf_dirty: bool,
    label_dirty: bool,

    shelf: Vec<ShelfEntry>,
    previews: HashMap<i64, HtmlCanvasElement>,
    pings: Vec<Rc<Ping>>,
    last_ping: HashMap<Option<Layer>, Rc<Ping>>,
    nudge: Option<Rc<Ping>>,
    nudge_timer: i32,
    ping_loop: bool,

    toast_undo: Option<Out>,
    toast_timer: i32,
    view_timer: i32,
    layers_queued: bool,
    flush_queued: bool,

    /// This tab holds the private board's lock and may write it.
    writer: bool,
    /// Resolves the never-ending lock callback, which lets the lock go.
    lock_release: Option<Function>,
    damaged_shown: bool,

    ws: Option<WebSocket>,
    /// Storage for when localStorage refuses (`lsGet`/`lsSet`'s map).
    mem: HashMap<String, String>,
    /// The open text box, as `input` opened it, for `placeEditor` while `input` is busy.
    editor: Option<Editor>,
    fx: Fx,
    d: Dom,
}

impl Page {
    fn new() -> Option<Page> {
        let base: HtmlCanvasElement = by_id("base")?;
        let live: HtmlCanvasElement = by_id("live")?;
        let spare = canvas();
        let d = Dom {
            body: doc().body()?,
            bctx: ctx2d(&base)?,
            lctx: ctx2d(&live)?,
            sctx: ctx2d(&spare)?,
            base,
            live,
            spare,
            board: html("board")?,
            paper: html("paper")?,
            ta: by_id("textEdit")?,
            toast: html("toast")?,
            msg: html("msg")?,
            status: html("status")?,
            who: html("who")?,
            on_open: sock_handler(|a, n, _| a.sock_open(n)),
            on_message: sock_handler(|a, n, e| {
                if let Some(text) = e.dyn_ref::<MessageEvent>().and_then(|m| m.data().as_string()) {
                    let App { conn, inp, page } = a;
                    conn.frame(page, inp, n, &text);
                    inp.sel_check(page);
                }
            }),
            on_close: sock_handler(|a, n, _| {
                let App { conn, inp, page } = a;
                conn.closed(page, inp, n);
            }),
        };
        let fx = Fx::new(by_id("fx")?);
        let my_name = names::clean_name(&recall("wb.name").unwrap_or_default());
        let my_name = if my_name.is_empty() { names::roll_name(random(), random()) } else { my_name };
        let my_hue = recall("wb.hue").map(|v| hue_of(&v));
        Some(Page {
            board: Board::default(),
            view: View::default(),
            redraw: Redraw::default(),
            bounds: Bounds::default(),
            local: Local::default(),
            mine: vec![],
            hidden: IndexSet::new(),
            scene: Scene::default(),
            scene_dirty: false,
            over: None,
            edit: false,
            me: None,
            is_local: false,
            online: false,
            ready: false,
            key: String::new(),
            people: 0,
            on: vec![],
            tool: Tool::Pen,
            color: None,
            my_color: None,
            width: 0.004,
            fill: false,
            heads: Heads::End,
            grid: Grid::Board,
            style: Style::default(),
            shape_pick: Tool::Shape(BoxKind::Rect),
            can_group: false,
            can_ungroup: false,
            ask: ShareGate::default(),
            my_name,
            my_hue,
            outbox: vec![],
            who_dirty: false,
            share_dirty: false,
            shelf_dirty: false,
            label_dirty: false,
            shelf: vec![],
            previews: HashMap::new(),
            pings: vec![],
            last_ping: HashMap::new(),
            nudge: None,
            nudge_timer: 0,
            ping_loop: false,
            toast_undo: None,
            toast_timer: 0,
            view_timer: 0,
            layers_queued: false,
            flush_queued: false,
            writer: false,
            lock_release: None,
            damaged_shown: false,
            ws: None,
            mem: HashMap::new(),
            editor: None,
            fx,
            d,
        })
    }

    // ---- storage ----

    /// `lsGet`: localStorage, else what was kept here when it refused.
    fn ls_get(&self, k: &str) -> Option<String> {
        storage().and_then(|s| s.get_item(k).ok().flatten()).or_else(|| self.mem.get(k).cloned())
    }

    /// `lsSet`.
    fn ls_set(&mut self, k: &str, v: &str) -> bool {
        self.mem.insert(k.into(), v.into());
        storage().is_some_and(|s| s.set_item(k, v).is_ok())
    }

    fn ls_remove(&mut self, k: &str) {
        self.mem.remove(k);
        if let Some(s) = storage() {
            let _ = s.remove_item(k);
        }
    }

    // ---- sending ----

    /// `send`: to the private board's outbox, or to the current socket when it is open.
    fn out(&mut self, o: Out) {
        if self.is_local {
            self.outbox.push(o);
        } else if let Some(ws) = &self.ws {
            if ws.ready_state() == WebSocket::OPEN {
                let _ = ws.send_with_str(&o.frame());
            }
        }
    }

    fn open_socket(&mut self, n: u64) {
        let loc = win().location();
        let proto = if loc.protocol().as_deref() == Ok("https:") { "wss:" } else { "ws:" };
        let url = format!("{}//{}/ws", proto, loc.host().unwrap_or_default());
        let Ok(ws) = WebSocket::new(&url) else { return };
        let _ = Reflect::set(&ws, &JsValue::from_str("wbSock"), &JsValue::from_f64(n as f64));
        ws.set_onopen(Some(&self.d.on_open));
        ws.set_onmessage(Some(&self.d.on_message));
        ws.set_onclose(Some(&self.d.on_close));
        self.ws = Some(ws);
    }

    // ---- names ----

    /// `nameOf`.
    fn name_of(&self, l: Option<Layer>) -> String {
        let n = l.and_then(|l| self.board.layers.get(&l)).and_then(|x| x.n.clone()).filter(|n| !n.is_empty());
        match (n, l) {
            (Some(n), _) => n,
            (None, Some(l)) => format!("Guest {l}"),
            (None, None) => "Guest".into(),
        }
    }

    fn layer_c(&self, l: Option<Layer>) -> Option<String> {
        l.and_then(|l| self.board.layers.get(&l)).map(|x| x.c.clone()).filter(|c| !c.is_empty())
    }

    fn is_hidden_layer(&self, l: Option<Layer>) -> bool {
        l.is_some_and(|l| self.hidden.contains(&l))
    }

    // ---- the base canvas ----

    /// `redrawBase(direct)`.
    fn base_redraw(&mut self, direct: bool) {
        self.scene_dirty = true;
        self.fresh_scene();
        let gen = self.redraw.start(direct, self.view.cur());
        if direct {
            let rv = self.redraw.rv;
            let _ = self.d.bctx.set_transform_m(self.view.ctx_transform(rv));
            self.stretch_base();
            render::Surface::wipe(&mut self.d.bctx.clone());
        } else {
            let _ = self.d.sctx.set_transform_m(self.view.ctx_transform(self.redraw.target));
            render::Surface::wipe(&mut self.d.sctx.clone());
        }
        self.step(gen);
    }

    /// `step(gen)`: one frame's slice of the redraw.
    fn step(&mut self, gen: u64) {
        let mut bctx = self.d.bctx.clone();
        let mut sctx = self.d.sctx.clone();
        let target = self.redraw.target;
        self.fresh_scene();
        let Page { redraw, board, view, bounds, hidden, scene, .. } = self;
        let strokes = &board.strokes;
        let r = redraw.step(gen, strokes.len(), BUDGET_MS, now, |i, buf| {
            let s = &strokes[i];
            if s.l.is_some_and(|l| hidden.contains(&l)) {
                return;
            }
            let ctx = if buf { &mut sctx } else { &mut bctx };
            render::draw_full(ctx, s, view, Some(target), bounds, scene);
        });
        match r {
            Step::More => raf(move || {
                with(|a| a.page.step(gen));
            }),
            Step::Swap => {
                render::blit(&self.d.bctx, &self.d.spare);
                let _ = self.d.bctx.set_transform_m(self.view.ctx_transform(self.redraw.rv));
                self.stretch_base();
            }
            Step::Stale | Step::Done => {}
        }
    }

    fn kick(&mut self) {
        if let Some(g) = self.redraw.kick() {
            raf(move || {
                with(|a| a.page.step(g));
            });
        }
    }

    /// `insertStroke(s, defer)`.
    fn insert(&mut self, s: Item, defer: bool) {
        let hidden = self.is_hidden_layer(s.l);
        let id = s.id.clone();
        // A box with lines on it moves them; a line drawn under others may hop under theirs:
        // either repaints more than the one item, so the whole base redraws.
        let wide = match &s.sh {
            Some(Shape::Box { n, .. }) => self.board.strokes.iter().any(|o| o.sh.as_ref().is_some_and(|x| x.is_line() && x.touches(n))),
            Some(Shape::Line { .. }) => true,
            None => false,
        };
        if s.sh.is_some() {
            self.scene_dirty = true;
        }
        if let Some(placed) = self.board.insert(s) {
            let plan = placed.plan(self.redraw.cursor, self.redraw.buffered, defer);
            if wide && !hidden {
                self.base_redraw(false);
                self.changed_layers();
                return;
            }
            if plan.draw && !hidden {
                if let Some(item) = self.board.get(&id).cloned() {
                    let rv = self.redraw.rv;
                    self.fresh_scene();
                    render::draw_full(&mut self.d.bctx.clone(), &item, &self.view, Some(rv), &mut self.bounds, &self.scene);
                }
            }
            if plan.advance {
                self.redraw.cursor += 1;
            }
            if plan.kick {
                self.kick();
            }
            if plan.redraw {
                self.base_redraw(false);
            }
            self.changed_layers();
        }
    }

    /// `removeStroke(id)`.
    fn remove(&mut self, id: &str) -> bool {
        if !self.board.remove(id) {
            return false;
        }
        self.bounds.forget(id);
        self.base_redraw(false);
        self.changed_layers();
        true
    }

    /// Rebuilds the scene if the board or the hidden layers changed since.
    fn fresh_scene(&mut self) {
        if !self.scene_dirty {
            return;
        }
        let hidden = &self.hidden;
        self.scene = Scene::build(self.board.strokes.iter(), |s| s.l.is_some_and(|l| hidden.contains(&l)));
        self.scene_dirty = false;
    }

    fn stretch_base(&self) {
        let t = self.view.stretch(self.redraw.rv);
        let st = self.d.base.style();
        if t.is_empty() {
            let _ = st.remove_property("transform");
        } else {
            let _ = st.set_property("transform", &t);
        }
    }

    // ---- the view ----

    fn place_paper(&self) {
        let r = self.view.paper();
        let p = &self.d.paper;
        css(p, "left", &format!("{}px", r.left));
        css(p, "top", &format!("{}px", r.top));
        css(p, "width", &format!("{}px", r.width));
        css(p, "height", &format!("{}px", r.height));
        toggle(&self.d.body, "grid", self.grid != Grid::Board);
        if self.grid != Grid::Board {
            let (g, x, y) = self.view.grid_css();
            let _ = p.style().set_property("--gs", &format!("{g}px"));
            let _ = p.style().set_property("--gp", &format!("{x}px {y}px"));
        }
    }

    fn show_zoom(&self) {
        let c = self.view.chrome();
        set_text("zoomReset", &format!("{}%", c.percent));
        set_hidden("pgL", c.hide_l);
        set_hidden("pgR", c.hide_r);
        set_hidden("pgU", c.hide_u);
        set_hidden("pgD", c.hide_d);
        set_hidden("nav", c.nav.is_none());
        if let (Some(r), Some(b)) = (c.nav, html("navBox")) {
            css(&b, "left", &format!("{}%", r.left));
            css(&b, "top", &format!("{}%", r.top));
            css(&b, "width", &format!("{}%", r.width));
            css(&b, "height", &format!("{}%", r.height));
        }
    }

    /// `setView`'s and `zoomAt`'s ending: redraw what moved, then settle now or soon.
    fn view_moved(&mut self, changed: bool, settle: bool) {
        if changed {
            self.place_paper();
            self.stretch_base();
            self.live_redraw();
            self.place_editor();
            self.show_zoom();
        }
        clear_timeout(self.view_timer);
        if settle {
            self.settle();
        } else {
            self.view_timer = timeout(140, || {
                with(|a| a.page.settle());
            });
        }
    }

    /// `settleView`.
    fn settle(&mut self) {
        clear_timeout(self.view_timer);
        self.view_timer = 0;
        if self.redraw.rv != self.view.cur() {
            self.base_redraw(false);
        }
    }

    fn layout(&mut self) {
        let w = win();
        let iw = w.inner_width().ok().and_then(|v| v.as_f64()).unwrap_or(1.0);
        let ih = w.inner_height().ok().and_then(|v| v.as_f64()).unwrap_or(1.0);
        let (cw, ch) = self.view.layout(iw, ih, w.device_pixel_ratio());
        for c in [&self.d.base, &self.d.live, &self.d.spare] {
            c.set_width(cw);
            c.set_height(ch);
        }
        self.place_paper();
        self.show_zoom();
        self.base_redraw(true);
        self.live_redraw();
        self.place_editor();
    }

    // ---- the live canvas ----

    /// `redrawLive`, with the pings.
    fn live_redraw(&mut self) {
        let mut lctx = self.d.lctx.clone();
        let hidden = &self.hidden;
        render::redraw_live(&mut lctx, &self.view, self.board.live.values_mut(), |s| {
            s.l.is_some_and(|l| hidden.contains(&l))
        });
        self.draw_pings();
        self.draw_overlay();
    }

    /// The shape tool's ghost at half opacity, then the anchor dots.
    fn draw_overlay(&self) {
        let Some(o) = &self.over else { return };
        let mut ctx = self.d.lctx.clone();
        ctx.save();
        let _ = ctx.set_global_composite_operation("source-over");
        for (c, ops) in &o.ghosts {
            render::Surface::prep(&mut ctx, c);
            ctx.set_global_alpha(0.5);
            for op in ops {
                render::Surface::op(&mut ctx, op);
            }
        }
        ctx.set_global_alpha(1.0);
        let (u, bw) = (1.0 / self.view.zoom, self.view.bw);
        let px = |r: &[f64; 4]| (r[0] * bw, r[1] * bw, (r[2] - r[0]) * bw, (r[3] - r[1]) * bw);
        if let Some(r) = &o.marquee {
            let (x, y, w, h) = px(r);
            ctx.set_fill_style_str("rgba(43,124,255,0.08)");
            ctx.fill_rect(x, y, w, h);
            ctx.set_line_width(1.0 * u);
            ctx.set_stroke_style_str("#2b7cff");
            ctx.stroke_rect(x, y, w, h);
        }
        if let Some(r) = &o.sel {
            let (x, y, w, h) = px(r);
            let pad = 4.0 * u;
            let dash = js_sys::Array::of2(&JsValue::from_f64(4.0 * u), &JsValue::from_f64(3.0 * u));
            let _ = ctx.set_line_dash(&dash);
            ctx.set_line_width(1.5 * u);
            ctx.set_stroke_style_str("#2b7cff");
            ctx.stroke_rect(x - pad, y - pad, w + 2.0 * pad, h + 2.0 * pad);
            let _ = ctx.set_line_dash(&js_sys::Array::new());
        }
        for &(x, y, hot) in &o.dots {
            ctx.begin_path();
            let _ = ctx.arc(x * bw, y * bw, if hot { 6.0 } else { 4.0 } * u, 0.0, std::f64::consts::TAU);
            ctx.set_fill_style_str(if hot { "#2b7cff" } else { "#fff" });
            ctx.fill();
            ctx.set_line_width(1.5 * u);
            ctx.set_stroke_style_str("#2b7cff");
            ctx.stroke();
        }
        ctx.restore();
    }

    fn new_draw(&mut self, id: &str) {
        let mut lctx = self.d.lctx.clone();
        let bw = self.view.bw;
        let hidden = &self.hidden;
        if let Some(s) = self.board.live.get_mut(id) {
            let h = s.l.is_some_and(|l| hidden.contains(&l));
            render::draw_new(&mut lctx, s, bw, h);
        }
    }

    fn draw_pings(&self) {
        if self.pings.is_empty() {
            return;
        }
        let t = now();
        let ctx = &self.d.lctx;
        let u = 1.0 / self.view.zoom;
        let (bw, dpr) = (self.view.bw, self.view.dpr);
        for p in &self.pings {
            let age = t - p.t0.get();
            if age > PING_MS || self.is_hidden_layer(p.l) || p.pts.len() < 2 {
                continue;
            }
            let c = self.layer_c(p.l).unwrap_or_else(|| PING_COLOR.into());
            let (a, pulse) = geom::ping_glow(age);
            ctx.save();
            let _ = ctx.set_global_composite_operation("source-over");
            ctx.set_line_cap("round");
            ctx.set_line_join("round");
            ctx.set_stroke_style_str(&c);
            ctx.set_shadow_color(&c);
            ctx.set_shadow_blur(22.0 * dpr * pulse);
            ctx.begin_path();
            ctx.move_to(p.pts[0] * bw, p.pts[1] * bw);
            let mut i = 3;
            while i + 1 < p.pts.len() {
                ctx.line_to(p.pts[i] * bw, p.pts[i + 1] * bw);
                i += 3;
            }
            ctx.set_global_alpha(a * 0.35 * pulse);
            ctx.set_line_width(14.0 * u);
            ctx.stroke();
            ctx.set_global_alpha(a);
            ctx.set_line_width(4.0 * u);
            ctx.stroke();
            // the letter, on a disc at the shape's top left
            let (x, y, r) = (p.bb[0] * bw, p.bb[1] * bw, 13.0 * u);
            ctx.set_shadow_blur(8.0 * dpr);
            ctx.set_fill_style_str(&c);
            ctx.begin_path();
            let _ = ctx.arc(x, y, r, 0.0, std::f64::consts::PI * 2.0);
            ctx.fill();
            ctx.set_shadow_blur(0.0);
            ctx.set_fill_style_str("#fff");
            ctx.set_font(&format!("700 {}px {}", 15.0 * u, FONT));
            ctx.set_text_align("center");
            ctx.set_text_baseline("middle");
            let _ = ctx.fill_text(&p.k, x, y + u);
            ctx.restore();
        }
    }

    /// `showPings`: the glow fades over frames, then the loop stops.
    fn show_pings(&mut self) {
        if self.ping_loop {
            return;
        }
        self.ping_loop = true;
        raf(ping_tick);
    }

    fn add_ping(&mut self, l: Option<Layer>, k: String, pts: Vec<f64>) {
        let k: String = {
            let first: String = k.chars().take(1).collect();
            if first.is_empty() { "?".into() } else { first }
        };
        let bb = geom::ping_box(&pts);
        let p = Rc::new(Ping { l, k, pts, t0: Cell::new(now()), bb });
        self.pings.push(p.clone());
        if self.pings.len() > 8 {
            self.pings.remove(0);
        }
        self.last_ping.insert(l, p.clone());
        if l != self.me {
            // a nudge, never a forced move
            self.nudge = Some(p);
            clear_timeout(self.nudge_timer);
            self.nudge_timer = timeout(NUDGE_MS, || {
                with(|a| {
                    a.page.nudge = None;
                    a.page.who_dirty = true;
                });
            });
        }
        self.who_dirty = true;
        self.show_pings();
    }

    /// `jumpToPing`.
    fn jump_to_ping(&mut self, p: Rc<Ping>) {
        let cx = (p.bb[0] + p.bb[2]) / 2.0;
        let cy = (p.bb[1] + p.bb[3]) / 2.0;
        let z = self.view.zoom;
        let (vw, vh) = (self.view.vis_w(z), self.view.vis_h(z));
        self.move_view(z, cx - vw / 2.0, cy - vh / 2.0, true);
        p.t0.set(now());
        if !self.pings.iter().any(|q| Rc::ptr_eq(q, &p)) {
            self.pings.push(p);
        }
        self.show_pings();
    }

    fn move_view(&mut self, z: f64, x: f64, y: f64, settle: bool) {
        let changed = self.view.set(z, x, y);
        self.view_moved(changed, settle);
    }

    fn zoom_by(&mut self, f: f64, sx: f64, sy: f64, settle: bool) {
        let changed = self.view.zoom_at(f, sx, sy);
        self.view_moved(changed, settle);
    }

    // ---- the text box ----

    fn place_editor(&self) {
        let Some(ed) = &self.editor else { return };
        let (left, top, px, width) = input::editor_box(ed, &self.view);
        let ta: &HtmlElement = &self.d.ta;
        css(ta, "left", &format!("{left}px"));
        css(ta, "top", &format!("{top}px"));
        css(ta, "font-size", &format!("{px}px"));
        css(ta, "color", &ed.c);
        css(ta, "width", &format!("{width}px"));
        css(ta, "height", "auto");
        let h = (self.d.ta.scroll_height() as f64).max(px * LINE_HEIGHT);
        css(ta, "height", &format!("{h}px"));
    }

    // ---- the private board ----

    /// `saveLocal`.
    fn save_now(&mut self) {
        if !self.is_local || !self.writer {
            return;
        }
        match self.local.save(&self.board, self.ls_get(LOCAL_KEY).as_deref()) {
            Save::Unchanged => {}
            Save::Write { sig, text } => {
                let ok = storage().is_some_and(|s| s.set_item(LOCAL_KEY, &text).is_ok());
                if ok {
                    self.mem.remove(LOCAL_KEY);
                }
                if self.local.saved(sig, &text, ok) {
                    self.show_toast(
                        "This board is too big to keep in this browser. Share it or it will be lost on reload.",
                        None,
                        8000,
                    );
                }
            }
            Save::Elsewhere => self.reload_local(),
            Save::Damaged => self.show_damaged(),
        }
    }

    /// Another tab wrote the private board: show what it wrote.
    fn reload_local(&mut self) {
        self.board.drop_all();
        self.board.pending.clear();
        self.bounds.clear();
        let layer = self.board.layers.get(&1).cloned();
        let stored = self.ls_get(LOCAL_KEY);
        let (local, mine) = Local::load(stored.as_deref(), &mut self.board);
        if let Some(l1) = layer {
            self.board.layers.insert(1, l1);
        }
        self.local = local;
        self.mine = mine;
        self.shelf = self.local.shelf_list();
        self.base_redraw(true);
        self.live_redraw();
        self.changed_layers();
        self.changed_shelf();
    }

    /// The stored private board could not be read: offer it as a file, or a fresh start.
    fn show_damaged(&mut self) {
        if self.damaged_shown {
            return;
        }
        self.damaged_shown = true;
        let m = &self.d.msg;
        clear(m);
        let _ = m.append_child(&el("p", None, Some("This private board could not be read.")));
        let a = el("a", None, Some("Download it"));
        if let Some(text) = self.ls_get(LOCAL_KEY) {
            let parts = Array::of1(&JsValue::from_str(&text));
            let bag = BlobPropertyBag::new();
            bag.set_type("application/json");
            if let Ok(blob) = Blob::new_with_str_sequence_and_options(&parts, &bag) {
                if let Ok(url) = Url::create_object_url_with_blob(&blob) {
                    if let Some(a) = a.dyn_ref::<HtmlAnchorElement>() {
                        a.set_href(&url);
                        a.set_download("whiteboard-private.json");
                    }
                }
            }
        }
        let _ = m.append_child(&a);
        let b = button("Start over");
        listen(&b, "click", |_| {
            with(|a| {
                a.page.local.discard_damaged();
                a.page.damaged_shown = false;
                a.page.save_now();
                a.page.d.msg.set_hidden(true);
            });
        });
        let _ = m.append_child(&b);
        m.set_hidden(false);
    }

    // ---- toasts and messages ----

    fn show_toast(&mut self, text: &str, undo: Option<Out>, ms: u32) {
        clear_timeout(self.toast_timer);
        let t = &self.d.toast;
        clear(t);
        let _ = t.append_child(&el("span", None, Some(text)));
        toggle(t, "plain", undo.is_none());
        if undo.is_some() {
            let _ = t.append_child(&button("Undo"));
        }
        self.toast_undo = undo;
        t.set_hidden(false);
        let ms = if ms == 0 { 4000 } else { ms as i32 };
        self.toast_timer = timeout(ms, || {
            with(|a| a.page.d.toast.set_hidden(true));
        });
    }

    fn show_message(&mut self, text: &str, then: Then) {
        let m = &self.d.msg;
        clear(m);
        let _ = m.append_child(&el("p", None, Some(text)));
        match then {
            Then::NewBoard => {
                let a = el("a", None, Some("New board"));
                attr(&a, "href", "/");
                let _ = m.append_child(&a);
            }
            Then::Retry => {
                let b = button("Retry");
                listen(&b, "click", |_| create_board());
                let _ = m.append_child(&b);
            }
            Then::Private => {
                let a = el("a", None, Some("Back to your board"));
                attr(&a, "href", "/");
                let _ = m.append_child(&a);
            }
        }
        m.set_hidden(false);
    }

    // ---- panels ----

    fn open_panel(&mut self, p: Option<&str>) {
        self.open_menu(None);
        for id in ["layersPanel", "sharePanel", "shelfPanel", "whoPanel"] {
            set_hidden(id, p != Some(id));
        }
        attr(&self.d.who, "aria-expanded", if p == Some("whoPanel") { "true" } else { "false" });
        for (btn, panel) in [("shelfBtn", "shelfPanel"), ("layersBtn", "layersPanel"), ("shareBtn", "sharePanel")] {
            if let Some(b) = html(btn) {
                attr(&b, "aria-expanded", if p == Some(panel) { "true" } else { "false" });
            }
        }
        match p {
            Some("whoPanel") => self.who_dirty = true,
            Some("shelfPanel") => self.shelf_dirty = true,
            Some("layersPanel") => self.render_layers(),
            Some("sharePanel") => self.share_dirty = true,
            _ => {}
        }
    }

    /// Shows one of the toolbar's menus under its button, or none.
    fn open_menu(&mut self, m: Option<&str>) {
        for (btn, menu) in [("shapeBtn", "shapeMenu"), ("headBtn", "headMenu"), ("dashBtn", "dashMenu"), ("gridBtn", "gridMenu")] {
            let on = m == Some(menu);
            if let Some(b) = html(btn) {
                attr(&b, "aria-expanded", if on { "true" } else { "false" });
                if let (true, Some(el)) = (on, html(menu)) {
                    el.set_hidden(false);
                    let r = b.get_bounding_client_rect();
                    let vw = win().inner_width().ok().and_then(|v| v.as_f64()).unwrap_or(r.right());
                    let left = r.left().min(vw - el.offset_width() as f64 - 8.0).max(8.0);
                    css(&el, "left", &format!("{left}px"));
                    css(&el, "top", &format!("{}px", r.bottom() + 6.0));
                    continue;
                }
            }
            set_hidden(menu, !on);
        }
    }

    fn toggle_menu(&mut self, id: &str) {
        self.open_menu(if is_hidden(id) { Some(id) } else { None });
    }

    /// The share question, opened by "Share this board". Cancel is focused, so Enter is safe.
    fn ask_share(&mut self) {
        if self.ask.ask() {
            set_hidden("shareAsk", false);
        }
        if let Some(b) = html("shareCancel") {
            let _ = b.focus();
        }
    }

    /// Closes the share question; `true` only for Share, the one answer that contacts the relay.
    fn answer_share(&mut self, a: Answer) -> bool {
        set_hidden("shareAsk", true);
        let go = self.ask.answer(a);
        if let Some(b) = html("shareNow") {
            let _ = b.focus();
        }
        go
    }

    fn toggle_panel(&mut self, id: &str) {
        self.open_panel(if is_hidden(id) { Some(id) } else { None });
    }

    /// `layersChanged`: the layers panel, next frame, if it is open.
    fn changed_layers(&mut self) {
        if self.layers_queued || is_hidden("layersPanel") {
            return;
        }
        self.layers_queued = true;
        raf(|| {
            with(|a| {
                a.page.layers_queued = false;
                a.page.render_layers();
            });
        });
    }

    fn render_layers(&mut self) {
        if is_hidden("layersPanel") {
            return;
        }
        let Some(list) = html("layerList") else { return };
        let me = self.me;
        let mut rows: Vec<&LayerInfo> = self.board.layers.values().collect();
        rows.sort_by(|a, b| {
            if Some(a.l) == me {
                std::cmp::Ordering::Less
            } else if Some(b.l) == me {
                std::cmp::Ordering::Greater
            } else {
                a.l.cmp(&b.l)
            }
        });
        clear(&list);
        for li_of in &rows {
            let l = li_of.l;
            let n = self.board.count(l);
            let off = self.hidden.contains(&l);
            let li = el("li", None, None);
            if n == 0 && !li_of.del && Some(l) != me {
                toggle(&li, "dim", true);
            }
            if off {
                toggle(&li, "off", true);
            }
            let d = el("span", Some("dot"), None);
            css(&d, "background", &li_of.c);
            let who = el("span", Some("who"), None);
            let _ = who.append_child(&bdi(&self.name_of(Some(l))));
            if Some(l) == me {
                let _ = who.append_with_str_1(" (you)");
            }
            let items = format!("{n} item{}{}", if n == 1 { "" } else { "s" }, if li_of.del { " · deleted" } else { "" });
            let _ = who.append_child(&el("small", None, Some(&items)));
            let _ = li.append_child(&d);
            let _ = li.append_child(&who);
            let name = net::Host::layer_name(self, l);
            let show = if off { "Show" } else { "Hide" };
            let _ = li.append_child(&layer_button(show, "eye", l, &format!("{show} {name} layer (only for me)")));
            if self.edit {
                if li_of.del {
                    let _ = li.append_child(&layer_button("Restore", "lr", l, &format!("Restore {name} layer for everyone")));
                }
                if n > 0 {
                    let b = layer_button("Delete", "ld", l, &format!("Delete {name} layer for everyone"));
                    toggle(&b, "danger", true);
                    let _ = li.append_child(&b);
                }
            }
            let _ = list.append_child(&li);
        }
        if rows.is_empty() {
            let _ = list.append_child(&el("li", Some("dim"), Some("No layers yet.")));
        }
    }

    /// `shelfChanged`.
    fn changed_shelf(&mut self) {
        let n = self.shelf.len();
        set_text("shelfN", &if n > 0 { n.to_string() } else { String::new() });
        let shelf = &self.shelf;
        self.previews.retain(|k, _| shelf.iter().any(|s| s.k == *k));
        self.shelf_dirty = true;
    }

    /// `renderShelf`. A missing preview is asked for (once per socket, by `conn`).
    fn render_shelf(&mut self, conn: &mut Conn) {
        if is_hidden("shelfPanel") {
            return;
        }
        let len = self.shelf.len();
        set_text(
            "shelfHint",
            &if len > 0 {
                format!(
                    "{len} of 5 crumpled boards kept. Bringing one back crumples the board in use in its place.{}",
                    if len >= 5 { " Crumpling another throws the oldest away." } else { "" }
                )
            } else {
                "No crumpled boards yet. Crumple wipes the board for everyone and keeps it here.".into()
            },
        );
        let Some(list) = html("shelfList") else { return };
        clear(&list);
        let mut asks = vec![];
        for i in (0..len).rev() {
            let ShelfEntry { k, n } = self.shelf[i].clone();
            let li = el("li", None, None);
            let pv = match self.previews.get(&k) {
                Some(c) => c.clone(),
                None => {
                    let c = canvas();
                    c.set_width(PV_W);
                    c.set_height(PV_H);
                    if let Some(o) = conn.ask(k) {
                        asks.push(o);
                    }
                    c
                }
            };
            let label = if i == len - 1 { "Latest".to_string() } else { format!("{} before", len - 1 - i) };
            let who = el("span", Some("who"), Some(&label));
            let _ = who.append_child(&el("small", None, Some(&format!("{n} item{}", if n == 1 { "" } else { "s" }))));
            let b = button("Bring back");
            attr(&b, "data-k", &k.to_string());
            attr(&b, "aria-label", &format!("Bring back crumpled board with {n} items"));
            let _ = li.append_child(&pv);
            let _ = li.append_child(&who);
            let _ = li.append_child(&b);
            let _ = list.append_child(&li);
        }
        for o in asks {
            self.out(o);
        }
    }

    /// `onSheetView`'s drawing: all nine pages, small.
    fn draw_sheet(&mut self, k: i64, a: Vec<Item>) {
        if !self.shelf.iter().any(|s| s.k == k) {
            return;
        }
        let c = canvas();
        c.set_width(PV_W);
        c.set_height(PV_H);
        let Some(mut ctx) = ctx2d(&c) else { return };
        let bw = self.view.bw;
        let k2 = PV_W as f64 / (WW * bw);
        let _ = ctx.set_transform(k2, 0.0, 0.0, k2, -WX * bw * k2, -WY * bw * k2);
        let mut scratch = Bounds::default();
        let sc = Scene::build(a.iter(), |_| false);
        for s in &a {
            render::draw_full(&mut ctx, s, &self.view, None, &mut scratch, &sc);
        }
        self.previews.insert(k, c);
        self.shelf_dirty = true;
    }

    /// `fillShare`.
    fn fill_share(&self, conn: &Conn) {
        let local = self.is_local;
        set_hidden("privRow", !local);
        set_hidden("viewRow", local);
        if let Some(p) = html("newBoard").and_then(|e| e.parent_element()).and_then(|e| e.dyn_into::<HtmlElement>().ok()) {
            p.set_hidden(local);
        }
        if local {
            set_hidden("editRow", true);
            return;
        }
        set_hidden("editRow", !self.edit);
        let at = |k: &str| format!("{}/b/{}", origin(), k);
        let (e, v) = conn.share_keys();
        if let Some(i) = by_id::<HtmlInputElement>("editLink") {
            i.set_value(&e.map(at).unwrap_or_default());
        }
        if let Some(i) = by_id::<HtmlInputElement>("viewLink") {
            i.set_value(&v.map(at).unwrap_or_default());
        }
    }

    /// `renderWho`: the people button, and the panel when it is open.
    fn render_who(&self) {
        let who = &self.d.who;
        toggle(who, "ping", self.nudge.is_some());
        let dot = who.query_selector(".dot").ok().flatten().and_then(|d| d.dyn_into::<HtmlElement>().ok());
        let text = html("whoText");
        if let Some(n) = &self.nudge {
            let c = self.layer_c(n.l).unwrap_or_else(|| "#e67e22".into());
            if let Some(d) = &dot {
                css(d, "background", &c);
            }
            let _ = who.style().remove_property("color");
            if let Some(t) = &text {
                clear(t);
                let _ = t.append_with_str_1(&format!("{} · ", n.k));
                let _ = t.append_child(&bdi(&self.name_of(n.l)));
                let _ = t.append_with_str_1(" is pointing");
            }
        } else {
            if let Some(d) = &dot {
                css(d, "background", self.my_color.as_deref().unwrap_or("#888"));
            }
            if let Some(t) = &text {
                clear(t);
                if self.is_local {
                    let _ = t.append_child(&bdi(&self.my_name));
                    let _ = t.append_with_str_1(" · private");
                } else {
                    if self.edit && self.me.is_some() {
                        let _ = t.append_child(&bdi(&self.my_name));
                    } else {
                        let _ = t.append_with_str_1("Watching");
                    }
                    let _ = t.append_with_str_1(&format!(" · {} here", self.people.max(1)));
                }
            }
        }
        if is_hidden("whoPanel") {
            return;
        }
        let active = doc().active_element();
        if let Some(i) = by_id::<HtmlInputElement>("nameIn") {
            if active.as_ref() != Some(i.as_ref()) {
                i.set_value(&self.my_name);
            }
        }
        if let (Some(h), Some(i)) = (self.my_hue, by_id::<HtmlInputElement>("hueIn")) {
            if active.as_ref() != Some(i.as_ref()) {
                i.set_value(&h.to_string());
            }
        }
        let me = self.me;
        let mut rows: Vec<&LayerInfo> = self
            .board
            .layers
            .values()
            .filter(|x| self.on.contains(&x.l) || self.last_ping.contains_key(&Some(x.l)) || Some(x.l) == me)
            .collect();
        let when = |x: &LayerInfo| self.last_ping.get(&Some(x.l)).map_or(0.0, |p| p.t0.get());
        rows.sort_by(|a, b| {
            when(b).partial_cmp(&when(a)).unwrap_or(std::cmp::Ordering::Equal).then_with(|| {
                if Some(a.l) == me {
                    std::cmp::Ordering::Less
                } else if Some(b.l) == me {
                    std::cmp::Ordering::Greater
                } else {
                    a.l.cmp(&b.l)
                }
            })
        });
        let Some(list) = html("whoList") else { return };
        clear(&list);
        for x in rows {
            let online = self.on.contains(&x.l);
            let li = el("li", if online { None } else { Some("away") }, None);
            let d = el("span", Some("dot"), None);
            css(&d, "background", &x.c);
            let w = el("span", Some("who"), None);
            let name = self.name_of(Some(x.l));
            let _ = w.append_child(&bdi(&name));
            if Some(x.l) == me {
                let _ = w.append_with_str_1(" (you)");
            }
            if !online {
                let _ = w.append_child(&el("small", None, Some("left")));
            }
            let _ = li.append_child(&d);
            let _ = li.append_child(&w);
            if let Some(p) = self.last_ping.get(&Some(x.l)) {
                let b = button(&format!("Go to {}", p.k));
                attr(&b, "aria-label", &format!("Go to ping {} from {}", p.k, name));
                attr(&b, "data-l", &x.l.to_string());
                let _ = li.append_child(&b);
            }
            let _ = list.append_child(&li);
        }
        let watching = self.people - self.on.len() as i64;
        if watching > 0 {
            let _ = list.append_child(&el(
                "li",
                Some("away"),
                Some(&format!("{watching} more watching or in another tab")),
            ));
        }
    }

    // ---- the toolbar ----

    fn build_swatches(&mut self) {
        let Some(sw) = html("swatches") else { return };
        clear(&sw);
        let Some(mine) = self.my_color.clone() else { return };
        let mut list = vec![mine.clone()];
        list.extend(FIXED_COLORS.iter().filter(|c| !c.eq_ignore_ascii_case(&mine)).map(|c| c.to_string()));
        for c in &list {
            let b = el("button", Some("sw"), None);
            attr(&b, "type", "button");
            attr(&b, "data-c", c);
            css(&b, "background", c);
            let label = if *c == mine { "My colour".to_string() } else { geom::color_name(c).map_or(c.clone(), str::to_string) };
            attr(&b, "aria-label", &label);
            let _ = sw.append_child(&b);
        }
        if !self.color.as_ref().is_some_and(|c| list.contains(c)) {
            self.color = Some(mine);
        }
        self.mark_tools();
    }

    fn mark_tools(&self) {
        let pressed = |b: &Element, on: bool| {
            toggle(b, "on", on);
            attr(b, "aria-pressed", if on { "true" } else { "false" });
        };
        if let Ok(all) = doc().query_selector_all("#bar [data-c]") {
            for i in 0..all.length() {
                if let Some(b) = all.item(i).and_then(|n| n.dyn_into::<Element>().ok()) {
                    let on = b.get_attribute("data-c").is_some_and(|c| Some(&c) == self.color.as_ref());
                    pressed(&b, on);
                }
            }
        }
        for (id, t) in [
            ("penBtn", Tool::Pen),
            ("eraser", Tool::Erase),
            ("textBtn", Tool::Text),
            ("pingBtn", Tool::Ping),
            ("moveBtn", Tool::Move),
            ("selectBtn", Tool::Select),
            ("bucketBtn", Tool::Bucket),
        ] {
            if let Some(b) = html(id) {
                pressed(&b, self.tool == t);
            }
        }
        let on_shape = matches!(self.tool, Tool::Shape(_) | Tool::Line);
        if let Some(b) = html("shapeBtn") {
            pressed(&b, on_shape);
            let pick = shape_key(if on_shape { self.tool } else { self.shape_pick });
            if let Some(svg) = doc().query_selector(&format!("#shapeMenu [data-shape={pick}] svg")).ok().flatten() {
                b.set_inner_html(&svg.outer_html());
            }
        }
        let checked = |b: &Element, on: bool| attr(b, "aria-checked", if on { "true" } else { "false" });
        for (sel, key, cur) in [
            ("#shapeMenu [data-shape]", "data-shape", if on_shape { shape_key(self.tool) } else { "" }),
            ("#headMenu [data-head]", "data-head", self.heads.key()),
            ("#gridMenu [data-grid]", "data-grid", self.grid.key()),
            ("#dashMenu [data-dash]", "data-dash", self.style.d.menu_key()),
        ] {
            if let Ok(all) = doc().query_selector_all(sel) {
                for i in 0..all.length() {
                    if let Some(b) = all.item(i).and_then(|n| n.dyn_into::<Element>().ok()) {
                        checked(&b, b.get_attribute(key).as_deref() == Some(cur));
                    }
                }
            }
        }
        if let Some(b) = html("fillBtn") {
            checked(&b, self.fill);
        }
        if let (Some(b), Some(svg)) = (
            html("headBtn"),
            doc().query_selector(&format!("#headMenu [data-head={}] svg", self.heads.key())).ok().flatten(),
        ) {
            b.set_inner_html(&svg.outer_html());
        }
        if let (Some(b), Some(svg)) = (
            html("dashBtn"),
            doc().query_selector(&format!("#dashMenu [data-dash={}] svg", self.style.d.menu_key())).ok().flatten(),
        ) {
            b.set_inner_html(&svg.outer_html());
        }
        if let (Some(b), Some(svg)) = (
            html("gridBtn"),
            doc().query_selector(&format!("#gridMenu [data-grid={}] svg", self.grid.key())).ok().flatten(),
        ) {
            b.set_inner_html(&svg.outer_html());
        }
        toggle(&self.d.body, "text", self.tool == Tool::Text);
        toggle(&self.d.body, "move", self.tool == Tool::Move);
        toggle(&self.d.body, "select", self.tool == Tool::Select);
        toggle(&self.d.body, "bucket", self.tool == Tool::Bucket);
        if let Some(b) = html("groupBtn") {
            attr(&b, "hidden", "");
            if self.tool == Tool::Select {
                let _ = b.remove_attribute("hidden");
            }
        }
        if let Ok(all) = doc().query_selector_all("#bar [data-w]") {
            for i in 0..all.length() {
                if let Some(b) = all.item(i).and_then(|n| n.dyn_into::<Element>().ok()) {
                    let on = b.get_attribute("data-w").is_some_and(|w: String| js_number(&w) == self.width);
                    pressed(&b, on);
                }
            }
        }
    }

    fn not_connected(&mut self) -> bool {
        if !self.is_local && !(self.online && self.ready) {
            self.show_toast("Not connected. Try again in a moment.", None, 0);
            return true;
        }
        false
    }

    /// `paperOfStrokes`: the window's view of the board, drawn in one go.
    fn paper_of_strokes(&mut self) -> Paper {
        self.fresh_scene();
        let view = &self.view;
        let strokes = &self.board.strokes;
        let hidden = &self.hidden;
        let scene = &self.scene;
        self.fx.paper_of_strokes(&self.d.base, |ctx, w| {
            if strokes.len() > 1500 {
                return;
            }
            let k = w / view.vw * view.zoom;
            let _ = ctx.set_transform(k, 0.0, 0.0, k, -view.pan_x * view.bw * k, -view.pan_y * view.bw * k);
            let mut c = ctx.clone();
            let mut scratch = Bounds::default();
            for s in strokes {
                if !s.l.is_some_and(|l| hidden.contains(&l)) {
                    render::draw_full(&mut c, s, view, Some(view.cur()), &mut scratch, scene);
                }
            }
        })
    }
}

/// `setTransform` from a matrix.
trait SetM {
    fn set_transform_m(&self, m: [f64; 6]) -> Result<(), JsValue>;
}

impl SetM for CanvasRenderingContext2d {
    fn set_transform_m(&self, m: [f64; 6]) -> Result<(), JsValue> {
        self.set_transform(m[0], m[1], m[2], m[3], m[4], m[5])
    }
}

fn layer_button(text: &str, act: &str, l: Layer, label: &str) -> HtmlElement {
    let b = button(text);
    attr(&b, "data-act", act);
    attr(&b, "data-l", &l.to_string());
    attr(&b, "aria-label", label);
    b
}

/// `clamp(Number(v) || 0, 0, 359)`.
fn hue_of(v: &str) -> f64 {
    let n = js_number(v);
    let n = if n.is_nan() { 0.0 } else { n };
    n.clamp(0.0, 359.0)
}

/// A socket handler: the socket's number comes from the event's target.
fn sock_handler(f: impl Fn(&mut App, u64, &Event) + 'static) -> Function {
    let c = Closure::<dyn FnMut(Event)>::new(move |e: Event| {
        let n = e.target().map_or(0.0, |t| num(&t, "wbSock")) as u64;
        with(|a| f(a, n, &e));
    });
    c.into_js_value().unchecked_into()
}

fn ping_tick() {
    let more = with(|a| {
        let p = &mut a.page;
        let t = now();
        p.pings.retain(|q| t - q.t0.get() <= PING_MS);
        p.live_redraw();
        if p.pings.is_empty() {
            p.ping_loop = false;
            false
        } else {
            true
        }
    });
    if more == Some(true) {
        raf(ping_tick);
    } else if more.is_none() {
        // busy: try again next frame
        raf(ping_tick);
    }
}

// ---- the hosts ------------------------------------------------------------------

impl net::Host for Page {
    type Paper = Paper;

    fn board(&mut self) -> &mut Board {
        &mut self.board
    }

    fn send(&mut self, o: Out) {
        self.out(o);
    }

    fn sock_send(&mut self, frame: String) {
        if let Some(ws) = &self.ws {
            if ws.ready_state() == WebSocket::OPEN {
                let _ = ws.send_with_str(&frame);
            }
        }
    }

    fn schedule(&mut self, ms: u32, t: Timer) {
        timeout(ms as i32, move || {
            with(|a| a.timer(t));
        });
    }

    fn insert_stroke(&mut self, s: Item, defer: bool) {
        self.insert(s, defer);
    }

    fn gone(&mut self, ids: &[String]) {
        for id in ids {
            self.bounds.forget(id);
        }
    }

    fn dropped(&mut self) {
        self.bounds.clear();
    }

    fn redraw_base(&mut self, full: bool) {
        self.base_redraw(full);
    }

    fn redraw_live(&mut self) {
        self.live_redraw();
    }

    fn draw_new(&mut self, id: &str) {
        self.new_draw(id);
    }

    fn set_drawing(&mut self, on: bool) {
        toggle(&self.d.body, "drawing", on);
    }

    fn set_status(&mut self, text: String) {
        if let Ok(Some(s)) = self.d.status.query_selector("span") {
            s.set_text_content(Some(&text));
        }
    }

    fn set_connected(&mut self, ok: bool) {
        toggle(&self.d.status, "ok", ok);
    }

    fn set_edit(&mut self, on: bool) {
        self.edit = on;
        toggle(&self.d.body, "edit", on);
    }

    fn my_color(&mut self, c: Option<String>) {
        if c.is_some() && (self.color.is_none() || self.color == self.my_color) {
            self.color = c.clone();
        }
        self.my_color = c;
        self.build_swatches();
    }

    fn set_shelf(&mut self, shelf: Vec<ShelfEntry>) {
        self.shelf = shelf;
        self.changed_shelf();
    }

    fn fill_share(&mut self) {
        self.share_dirty = true;
    }

    fn layers_changed(&mut self) {
        self.changed_layers();
    }

    fn render_who(&mut self) {
        self.who_dirty = true;
    }

    fn send_label(&mut self) {
        self.label_dirty = true;
    }

    fn layer_name(&self, l: Layer) -> String {
        if Some(l) == self.me {
            "Your".into()
        } else {
            format!("{}'s", self.name_of(Some(l)))
        }
    }

    fn toast(&mut self, text: &str, undo: Option<Out>, ms: u32) {
        self.show_toast(text, undo, ms);
    }

    fn message(&mut self, text: &str, then: Then) {
        self.show_message(text, then);
    }

    fn forget_local(&mut self, kept: &[Item]) {
        let mut b = Board::default();
        for i in kept {
            b.insert(i.clone());
        }
        // through the save guards (H3): never over a damaged board or another tab's newer one
        if let Some(text) = self.local.after_share(&b, self.ls_get(LOCAL_KEY).as_deref()) {
            self.ls_set(LOCAL_KEY, &text);
        }
        self.ls_remove(SHARING_KEY);
    }

    fn writer_done(&mut self) {
        if let Some(r) = self.lock_release.take() {
            let _ = r.call0(&JsValue::NULL);
        }
    }

    fn share_lost(&mut self) {
        // LOCAL_KEY still holds the whole private board: nothing was forgotten yet
        self.ls_remove(SHARING_KEY);
        let _ = win().location().replace("/");
    }

    fn share_refused(&mut self) {
        // LOCAL_KEY still holds the whole private board
        self.ls_remove(SHARING_KEY);
    }

    fn sheet_view(&mut self, k: i64, a: Vec<Item>) {
        self.draw_sheet(k, a);
    }

    fn ping(&mut self, l: Option<Layer>, k: String, pts: Vec<f64>) {
        self.add_ping(l, k, pts);
    }

    fn pings(&self) -> bool {
        !self.pings.is_empty()
    }

    fn paper_of_screen(&mut self) -> Paper {
        if self.redraw.rv == self.view.cur() && self.redraw.cursor >= self.board.strokes.len() {
            self.fx.paper_of_screen(&self.d.base, &self.d.live)
        } else {
            self.paper_of_strokes()
        }
    }

    fn crumple_fx(&mut self, tex: Paper) {
        self.fx.play(tex, false, 950.0, None);
    }

    fn hide_base(&mut self) {
        css(&self.d.base, "visibility", "hidden");
    }

    fn unfold_fx(&mut self, old: Option<Paper>) {
        if let Some(old) = old {
            self.fx.play(old, false, 450.0, None);
        }
        let paper = self.paper_of_strokes();
        // may run at once, inside this call: it must not borrow the app
        let base = self.d.base.clone();
        self.fx.play(paper, true, 800.0, Some(Box::new(move || css(&base, "visibility", ""))));
    }
}

impl input::Host for Page {
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
        Pen {
            edit: self.edit,
            me: self.me,
            tool: self.tool,
            color: self.color.clone(),
            my_color: self.my_color.clone(),
            width: self.width,
            fill: self.fill,
            heads: self.heads,
            style: self.style.clone(),
            snap: self.grid == Grid::Snap,
        }
    }

    fn scene(&mut self) -> &Scene {
        self.fresh_scene();
        &self.scene
    }

    fn sel_changed(&mut self, can_group: bool, can_ungroup: bool) {
        (self.can_group, self.can_ungroup) = (can_group, can_ungroup);
        if let Some(b) = html("groupBtn") {
            let (t, on) = match (can_group, can_ungroup) {
                (true, _) => ("Group the selection (Ctrl+G)", true),
                (false, true) => ("Ungroup (Shift+Ctrl+G)", true),
                _ => ("Group: select two or more things first", false),
            };
            attr(&b, "title", t);
            attr(&b, "aria-label", t);
            attr(&b, "aria-disabled", if on { "false" } else { "true" });
            attr(&b, "aria-pressed", if !can_group && can_ungroup { "true" } else { "false" });
        }
    }

    fn overlay(&mut self, o: Option<Overlay>) {
        let had = self.over.is_some();
        self.over = o;
        if had || self.over.is_some() {
            self.live_redraw();
        }
    }

    fn net(&self) -> Net {
        Net { online: self.online, ready: self.ready, local: self.is_local }
    }

    fn hidden(&self, l: Option<Layer>) -> bool {
        self.is_hidden_layer(l)
    }

    fn now(&self) -> f64 {
        now()
    }

    fn new_id(&mut self) -> String {
        new_id()
    }

    fn rect(&self) -> (f64, f64) {
        let r = self.d.board.get_bounding_client_rect();
        (r.left(), r.top())
    }

    fn bbox(&mut self, s: &Item) -> [f64; 4] {
        self.bounds.of(s, &mut self.d.bctx.clone())
    }

    fn send(&mut self, o: Out) {
        self.out(o);
    }

    fn set_view(&mut self, z: f64, x: f64, y: f64, settle: bool) {
        self.move_view(z, x, y, settle);
    }

    fn zoom_at(&mut self, f: f64, sx: f64, sy: f64, settle: bool) {
        self.zoom_by(f, sx, sy, settle);
    }

    fn settle_view(&mut self) {
        self.settle();
    }

    fn capture(&mut self, pointer: i32) {
        let _ = self.d.live.set_pointer_capture(pointer);
    }

    fn set_drawing(&mut self, on: bool) {
        toggle(&self.d.body, "drawing", on);
    }

    fn request_flush(&mut self) {
        if self.flush_queued {
            return;
        }
        self.flush_queued = true;
        raf(|| {
            with(|a| {
                a.page.flush_queued = false;
                let App { inp, page, .. } = a;
                inp.flush(page);
            });
        });
    }

    fn draw_new(&mut self, id: &str) {
        self.new_draw(id);
    }

    fn redraw_live(&mut self) {
        self.live_redraw();
    }

    fn insert_stroke(&mut self, s: Item) {
        self.insert(s, false);
    }

    fn remove_stroke(&mut self, id: &str) -> bool {
        self.remove(id)
    }

    fn send_ping(&mut self, pts: Vec<f64>, touch: bool) {
        match geom::ping_points(&pts, touch, self.view.bw * self.view.zoom) {
            None => self.show_toast("Circle something to ping it.", None, 2500),
            Some(pts) if self.online && self.ready => self.out(Out::Ping { pts }),
            Some(_) => self.show_toast("Not connected: the ping was not sent.", None, 2500),
        }
    }

    fn ping_done(&mut self) {
        self.tool = Tool::Pen;
        self.mark_tools();
    }

    fn save_local(&mut self) {
        self.save_now();
    }

    fn editor_show(&mut self, ed: &Editor, text: &str) {
        self.editor = Some(ed.clone());
        self.d.ta.set_value(text);
        self.d.ta.set_hidden(false);
        self.place_editor();
        let _ = self.d.ta.focus();
    }

    fn editor_text(&self) -> String {
        self.d.ta.value()
    }

    fn editor_hide(&mut self) {
        self.editor = None;
        self.d.ta.set_hidden(true);
        self.d.ta.set_value("");
        // fires `blur` at once; its handler finds the app busy and drops it
        let _ = self.d.ta.blur();
    }
}

// ---- the app ----------------------------------------------------------------------

struct App {
    conn: Conn,
    inp: Input,
    page: Page,
}

impl App {
    /// The page's copies of the connection's fields.
    fn sync(&mut self) {
        let App { conn, page, .. } = self;
        page.edit = conn.edit;
        page.me = conn.me;
        page.is_local = conn.local;
        page.online = conn.online;
        page.ready = conn.ready;
        page.people = conn.people;
        if page.key != conn.key {
            page.key = conn.key.clone();
        }
        if page.on != conn.on {
            page.on = conn.on.clone();
        }
    }

    /// After each dispatch: answer the private board's frames, then draw the panels that read
    /// the connection and send the label.
    fn after(&mut self) {
        loop {
            self.pump();
            self.sync();
            if std::mem::take(&mut self.page.label_dirty) {
                self.send_label();
                continue;
            }
            if std::mem::take(&mut self.page.shelf_dirty) {
                let App { conn, page, .. } = self;
                page.render_shelf(conn);
                continue;
            }
            break;
        }
        if std::mem::take(&mut self.page.who_dirty) {
            self.page.render_who();
        }
        if std::mem::take(&mut self.page.share_dirty) {
            self.page.fill_share(&self.conn);
        }
    }

    /// `localSend` for each frame in the outbox.
    fn pump(&mut self) {
        let App { conn, inp, page } = self;
        while !page.outbox.is_empty() {
            for o in std::mem::take(&mut page.outbox) {
                match o {
                    Out::Crumple => {
                        if let Some(m) = page.local.crumple(&mut page.board) {
                            page.mine.clear();
                            conn.handle(page, inp, m);
                        }
                    }
                    Out::Uncrumple { k } => {
                        if let Some((m, repend)) = page.local.uncrumple(&mut page.board, k) {
                            page.mine.clear();
                            conn.handle(page, inp, m);
                            repend.apply(&mut page.board);
                        }
                    }
                    Out::SheetView { k } => {
                        if let Some(m) = page.local.sheet_view(k) {
                            conn.handle(page, inp, m);
                        }
                    }
                    _ => {}
                }
                page.save_now();
            }
        }
    }

    /// `sendLabel`.
    fn send_label(&mut self) {
        let App { conn, inp, page } = self;
        let Some(me) = page.me.filter(|_| page.edit) else { return };
        let c = match page.my_hue {
            Some(h) => Some(geom::hue_hex(h)),
            None => page.board.layers.get(&me).map(|x| x.c.clone()).filter(|c| !c.is_empty()),
        };
        let Some(c) = c else { return };
        if page.is_local {
            let m = In::Name { l: me, n: Some(page.my_name.clone()), c: Some(c) };
            conn.handle(page, inp, m);
        } else {
            page.out(Out::Name { n: page.my_name.clone(), c });
        }
    }

    fn timer(&mut self, t: Timer) {
        let App { conn, page, .. } = self;
        match t {
            Timer::Reconnect => {
                if let Some(n) = conn.connect() {
                    page.open_socket(n);
                }
            }
            Timer::Bite(s) => conn.bite(page, s),
            Timer::Settled(s) => conn.settled(s),
        }
    }

    /// `onopen`: join with this browser's secret for the board.
    fn sock_open(&mut self, n: u64) {
        let current = self.page.ws.as_ref().map(|w| num(w, "wbSock") as u64);
        if current != Some(n) {
            return;
        }
        let k = local::who_key(&self.conn.key);
        let who = match self.page.ls_get(&k) {
            Some(w) if local::valid_secret(&w) => w,
            _ => {
                let mut bytes = [0u8; 24];
                if let Ok(c) = win().crypto() {
                    let _ = c.get_random_values_with_u8_array(&mut bytes);
                }
                let w = local::secret_from(&bytes);
                self.page.ls_set(&k, &w);
                w
            }
        };
        let frame = self.conn.join(&who).frame();
        if let Some(ws) = &self.page.ws {
            let _ = ws.send_with_str(&frame);
        }
    }

    /// `start(k)`: a board on the relay.
    fn start(&mut self, k: &str) {
        let App { conn, page, .. } = self;
        page.hidden = local::parse_hidden(page.ls_get(&local::hide_key(k)).as_deref());
        if let Some(n) = conn.start(page, k) {
            page.open_socket(n);
        }
    }

    /// `startLocal`'s loading: the private board from storage.
    fn load_local(&mut self, layer1: bool) {
        self.conn.start_local();
        self.sync();
        let page = &mut self.page;
        let c = page.my_hue.map(geom::hue_hex).unwrap_or_else(|| "#111111".into());
        if layer1 {
            page.board.layers.insert(1, LayerInfo { l: 1, c: c.clone(), del: false, n: Some(page.my_name.clone()) });
            page.my_color = Some(c.clone());
            page.color = Some(c);
        }
        let stored = page.ls_get(LOCAL_KEY);
        let (local, mine) = Local::load(stored.as_deref(), &mut page.board);
        page.local = local;
        page.mine = mine;
        page.shelf = page.local.shelf_list();
        if page.local.damaged() {
            // F1: say so at once, before the user draws on a board that cannot be saved.
            page.show_damaged();
        }
    }

    /// `startLocal`.
    fn start_local(&mut self) {
        self.load_local(true);
        let App { conn, page, .. } = self;
        toggle(&page.d.body, "edit", true);
        toggle(&page.d.body, "local", true);
        toggle(&page.d.status, "priv", true);
        page.build_swatches();
        page.base_redraw(true);
        page.live_redraw();
        conn.show_status(page, "");
        page.share_dirty = true;
        page.changed_layers();
        page.changed_shelf();
        page.who_dirty = true;
        let _ = win().set_interval_with_callback_and_timeout_and_arguments_0(
            Closure::<dyn FnMut()>::new(|| {
                with(|a| a.page.save_now());
            })
            .into_js_value()
            .unchecked_ref(),
            SAVE_MS,
        );
        listen(&win(), "pagehide", |_| {
            with(|a| a.page.save_now());
        });
        listen(&win(), "storage", |e| {
            let Some(e) = e.dyn_ref::<StorageEvent>() else { return };
            if e.key().as_deref() != Some(LOCAL_KEY) {
                return;
            }
            let v = e.new_value();
            with(|a| {
                if a.page.is_local && a.page.local.elsewhere(v.as_deref()) {
                    a.page.reload_local();
                }
            });
        });
        request_lock(page);
    }

    /// `boot`.
    fn boot(&mut self) {
        self.page.layout();
        let path = win().location().pathname().unwrap_or_default();
        match local::route(&path) {
            Route::Board(k) => self.start(&k),
            Route::Local => {
                let target = local::sharing_target(self.page.ls_get(SHARING_KEY).as_deref()).map(str::to_string);
                match target {
                    Some(k) => {
                        // a share was cut short: finish it
                        self.load_local(false);
                        self.conn.resume_share();
                        replace_path(&format!("/b/{k}"));
                        self.start(&k);
                    }
                    None => self.start_local(),
                }
            }
            Route::Missing => self.page.show_message("This board doesn't exist or has expired.", Then::NewBoard),
        }
    }

    /// `crumple`.
    fn crumple(&mut self) {
        let App { inp, page, .. } = self;
        if !page.edit || page.not_connected() {
            return;
        }
        if page.board.strokes.is_empty() {
            page.show_toast("Nothing to crumple: the board is blank.", None, 0);
            return;
        }
        if inp.editor.is_some() {
            inp.close_editor(page, true);
        }
        page.out(Out::Crumple);
    }

    fn zoom_step(&mut self, f: f64) {
        let p = &mut self.page;
        let (cx, cy) = (p.view.vw / 2.0, p.view.vh / 2.0);
        p.zoom_by(f, cx, cy, true);
    }

    fn zoom_home(&mut self) {
        let changed = self.page.view.zoom_home();
        self.page.view_moved(changed, true);
    }

    fn page_step(&mut self, di: f64, dj: f64) {
        let changed = self.page.view.page_step(di, dj);
        self.page.view_moved(changed, true);
    }

    fn bar(&mut self, b: &Element) {
        if b.id() == "groupBtn" {
            let App { inp, page, .. } = self;
            if page.can_group {
                inp.group_sel(page);
            } else if page.can_ungroup {
                inp.ungroup_sel(page);
            }
            return;
        }
        {
            let App { inp, page, .. } = self;
            inp.cancel_shape(page);
        }
        match b.id().as_str() {
            "shapeBtn" => return self.page.toggle_menu("shapeMenu"),
            "headBtn" => return self.page.toggle_menu("headMenu"),
            "gridBtn" => return self.page.toggle_menu("gridMenu"),
            "dashBtn" => return self.page.toggle_menu("dashMenu"),
            _ => self.page.open_menu(None),
        }
        if let Some(c) = b.get_attribute("data-c").filter(|c| !c.is_empty()) {
            self.page.color = Some(c);
            if !matches!(self.page.tool, Tool::Text | Tool::Bucket) {
                self.page.tool = Tool::Pen;
            }
            self.page.mark_tools();
            return;
        }
        if let Some(w) = b.get_attribute("data-w").filter(|w| !w.is_empty()) {
            self.page.width = js_number(&w);
            self.page.mark_tools();
            return;
        }
        let flip = |p: &mut Page, t: Tool| {
            p.tool = if p.tool == t { Tool::Pen } else { t };
            p.mark_tools();
        };
        match b.id().as_str() {
            "penBtn" => {
                self.page.tool = Tool::Pen;
                self.page.mark_tools();
            }
            "eraser" => flip(&mut self.page, Tool::Erase),
            "textBtn" => flip(&mut self.page, Tool::Text),
            "pingBtn" => flip(&mut self.page, Tool::Ping),
            "moveBtn" => flip(&mut self.page, Tool::Move),
            "selectBtn" => flip(&mut self.page, Tool::Select),
            "bucketBtn" => flip(&mut self.page, Tool::Bucket),
            "crumpleBtn" => self.crumple(),
            "shelfBtn" => self.page.toggle_panel("shelfPanel"),
            "layersBtn" => self.page.toggle_panel("layersPanel"),
            "shareBtn" => self.page.toggle_panel("sharePanel"),
            "zoomIn" => self.zoom_step(1.5),
            "zoomOut" => self.zoom_step(1.0 / 1.5),
            "zoomReset" => self.zoom_home(),
            "undo" => {
                let App { inp, page, .. } = self;
                inp.undo(page);
            }
            _ => {}
        }
    }

    fn set_name(&mut self, name: String) {
        self.page.my_name = name;
        if let Some(i) = by_id::<HtmlInputElement>("nameIn") {
            i.set_value(&self.page.my_name);
        }
        remember("wb.name", &self.page.my_name);
        self.page.label_dirty = true;
        self.page.who_dirty = true;
    }
}

/// F6: one tab writes the private board. Where Web Locks exist, the tab that holds `wb.local`
/// is the writer; another tab is read-only until it gets the lock. Without them, every tab
/// writes, and the storage check ([`Local::elsewhere`]) keeps them from losing each other's work.
fn request_lock(page: &mut Page) {
    let nav = win().navigator();
    let locks = Reflect::get(&nav, &JsValue::from_str("locks")).unwrap_or(JsValue::UNDEFINED);
    let request = Reflect::get(&locks, &JsValue::from_str("request")).ok().and_then(|f| f.dyn_into::<Function>().ok());
    let Some(request) = request.filter(|_| locks.is_object()) else {
        page.writer = true;
        return;
    };
    let opts = js_sys::Object::new();
    let _ = Reflect::set(&opts, &JsValue::from_str("ifAvailable"), &JsValue::TRUE);
    let locks2 = locks.clone();
    let request2 = request.clone();
    let cb = Closure::once_into_js(move |lock: JsValue| -> JsValue {
        if !lock.is_null() && !lock.is_undefined() {
            return hold_lock(false);
        }
        with(|a| {
            a.page.writer = false;
            a.conn.edit = false;
            net::Host::set_edit(&mut a.page, false);
            a.page.show_toast(
                "This private board is open in another tab. It is read-only here until that tab closes.",
                None,
                0,
            );
        });
        let wait = Closure::once_into_js(move |_lock: JsValue| -> JsValue { hold_lock(true) });
        let _ = request2.call2(&locks2, &JsValue::from_str(LOCK), &wait);
        JsValue::UNDEFINED
    });
    let _ = request.call3(&locks, &JsValue::from_str(LOCK), &opts, &cb);
}

/// The lock is ours: write, and keep it (a promise that ends only when a share lets it go).
fn hold_lock(waited: bool) -> JsValue {
    let mut release = None;
    let p = Promise::new(&mut |resolve, _| release = Some(resolve));
    with(|a| {
        a.page.writer = true;
        a.page.lock_release = release;
        if waited && a.page.is_local {
            a.page.reload_local();
            a.conn.edit = true;
            net::Host::set_edit(&mut a.page, true);
            a.page.show_toast("This tab can edit the private board now.", None, 0);
        }
    });
    p.into()
}

/// The shape menu's key for a tool (`data-shape`).
fn shape_key(t: Tool) -> &'static str {
    match t {
        Tool::Shape(k) => k.key(),
        Tool::Line => "line",
        _ => "",
    }
}

/// `shareNow`: make a board on the relay and move the private board to it.
fn share_now() {
    let go = with(|a| {
        let may_write = a.page.writer && !a.page.local.damaged();
        if !a.conn.share_begin(may_write) {
            if a.conn.local && !a.conn.uploading {
                if a.page.local.damaged() {
                    a.page.show_damaged();
                } else if !a.page.writer {
                    a.page.show_toast("This board is open in another tab. Share it from there.", None, 6000);
                }
            }
            return false;
        }
        if let Some(b) = by_id::<HtmlButtonElement>("shareNow") {
            b.set_disabled(true);
            b.set_text_content(Some("Sharing…"));
        }
        true
    });
    if go != Some(true) {
        return;
    }
    post_board(|made| {
        with(|a| {
            let reset = || {
                if let Some(b) = by_id::<HtmlButtonElement>("shareNow") {
                    b.set_disabled(false);
                    b.set_text_content(Some("Share this board"));
                }
            };
            let Made::Board(edit) = &made else {
                a.conn.share_failed();
                reset();
                if let Some(t) = made.share_failure() {
                    a.page.show_toast(t, None, 6000);
                }
                return;
            };
            a.page.save_now();
            a.conn.shared();
            a.sync();
            toggle(&a.page.d.body, "local", false);
            toggle(&a.page.d.status, "priv", false);
            a.page.ls_set(SHARING_KEY, edit);
            replace_path(&format!("/b/{edit}"));
            reset();
            // the writer's lock is kept until the share has made its last write (G5)
            a.start(edit);
        });
    });
}

/// `createBoard`: only reached from its own Retry button, as in app.js.
fn create_board() {
    with(|a| {
        a.page.d.msg.set_hidden(true);
        let App { conn, page, .. } = a;
        conn.show_status(page, "creating board");
    });
    post_board(|made| {
        with(|a| match &made {
            Made::Board(edit) => {
                replace_path(&format!("/b/{edit}"));
                a.start(edit);
            }
            _ => {
                if let Some((status, msg)) = made.create_failure() {
                    let App { conn, page, .. } = a;
                    conn.show_status(page, status);
                    page.show_message(msg, Then::Retry);
                }
            }
        });
    });
}

/// `fetch('/api/boards', { method: 'POST' })`, read into a [`Made`].
fn post_board(done: impl FnOnce(Made) + 'static) {
    let init = RequestInit::new();
    init.set_method("POST");
    let p = win().fetch_with_str_and_init("/api/boards", &init);
    let done = Rc::new(RefCell::new(Some(done)));
    let finish = move |m: Made| {
        if let Some(f) = done.borrow_mut().take() {
            f(m);
        }
    };
    let finish = Rc::new(finish);
    let (f1, f2) = (finish.clone(), finish);
    then(
        &p,
        move |res: JsValue| {
            let Ok(res) = res.dyn_into::<Response>() else { return f1(Made::of(None, None)) };
            let status = res.status();
            let Ok(text) = res.text() else { return f1(Made::of(Some(status), None)) };
            let f3 = f1.clone();
            then(
                &text,
                move |t: JsValue| f1(Made::of(Some(status), t.as_string().as_deref())),
                move |_| f3(Made::of(Some(status), None)),
            );
        },
        move |_| f2(Made::of(None, None)),
    );
}

// ---- wiring ---------------------------------------------------------------------------

fn wire(page: &Page) {
    let w = win();
    let live = &page.d.live;

    // pointers on the live canvas
    listen(live, "pointerdown", |e| {
        let Some(p) = e.dyn_ref::<PointerEvent>() else { return };
        let ptr = ptr_of(p);
        if with(|a| {
            let App { inp, page, .. } = a;
            if !(is_hidden("shapeMenu") && is_hidden("headMenu") && is_hidden("dashMenu") && is_hidden("gridMenu")) {
                page.open_menu(None);
            }
            let r = inp.down(page, &ptr);
            mark_pan(inp, page);
            r
        }) == Some(true)
        {
            e.prevent_default();
        }
    });
    listen(live, "pointermove", |e| {
        let Some(p) = e.dyn_ref::<PointerEvent>() else { return };
        let (ptr, co) = (ptr_of(p), coalesced(p));
        with(|a| {
            let App { inp, page, .. } = a;
            inp.moved(page, &ptr, &co);
        });
    });
    for (ev, up) in [("pointerup", true), ("pointercancel", false), ("lostpointercapture", false)] {
        listen(live, ev, move |e| {
            let Some(p) = e.dyn_ref::<PointerEvent>() else { return };
            let ptr = ptr_of(p);
            with(|a| {
                let App { inp, page, .. } = a;
                inp.end(page, &ptr, up);
                mark_pan(inp, page);
            });
        });
    }
    listen(live, "contextmenu", |e| e.prevent_default());

    // the wheel and Safari's pinch
    listen_active(&w, "wheel", |e| {
        if closest(&e, ".panel").is_some() {
            return;
        }
        let Some(we) = e.dyn_ref::<WheelEvent>() else { return };
        e.prevent_default();
        let wh = Wheel {
            dx: we.delta_x(),
            dy: we.delta_y(),
            line: we.delta_mode() == 1,
            zoom_key: we.ctrl_key() || we.meta_key(),
            cx: num(we, "clientX"),
            cy: num(we, "clientY"),
        };
        with(|a| {
            let App { inp, page, .. } = a;
            inp.wheel(page, &wh);
        });
    });
    listen_active(&w, "gesturestart", |e| {
        e.prevent_default();
        with(|a| {
            let App { inp, page, .. } = a;
            inp.gesture_start(page);
        });
    });
    listen_active(&w, "gesturechange", |e| {
        e.prevent_default();
        let (s, x, y) = (num(&e, "scale"), num(&e, "clientX"), num(&e, "clientY"));
        with(|a| {
            let App { inp, page, .. } = a;
            inp.gesture_change(page, s, x, y);
        });
    });
    listen_active(&w, "gestureend", |e| e.prevent_default());

    // the keyboard
    listen(&w, "keydown", |e| {
        let Some(k) = e.dyn_ref::<KeyboardEvent>() else { return };
        let in_input = e.target().is_some_and(|t| t.dyn_ref::<HtmlInputElement>().is_some());
        let key = input::key(&k.key(), k.ctrl_key(), k.meta_key(), k.shift_key(), k.alt_key(), in_input);
        if key.prevents() {
            e.prevent_default();
        }
        with(|a| match key {
            Key::None => {}
            Key::Undo | Key::Space | Key::Delete | Key::Group | Key::Ungroup => {
                let App { inp, page, .. } = a;
                inp.key(page, key);
                mark_pan(inp, page);
            }
            Key::ClosePanel => {
                let App { inp, page, .. } = a;
                inp.cancel_shape(page);
                inp.deselect(page);
                page.open_panel(None);
            }
            Key::ZoomStep(f) => a.zoom_step(f),
            Key::ZoomHome => a.zoom_home(),
            Key::PageStep(di, dj) => a.page_step(di, dj),
        });
    });
    listen(&w, "keyup", |e| {
        if e.dyn_ref::<KeyboardEvent>().is_some_and(|k| k.key() == " ") {
            with(|a| {
                a.inp.space_up();
                mark_pan(&a.inp, &a.page);
            });
        }
    });
    listen(&w, "blur", |_| {
        with(|a| {
            a.inp.space_up();
            mark_pan(&a.inp, &a.page);
        });
    });
    listen(&w, "resize", |_| {
        with(|a| a.page.layout());
    });
    listen(&doc(), "visibilitychange", |_| {
        if doc().visibility_state() != web_sys::VisibilityState::Hidden {
            return;
        }
        with(|a| {
            let App { inp, page, .. } = a;
            inp.hidden(page);
            page.save_now();
        });
    });

    // the text box
    let ta = &page.d.ta;
    listen(ta, "input", |_| {
        with(|a| a.page.place_editor());
    });
    listen(ta, "blur", |_| {
        with(|a| {
            let App { inp, page, .. } = a;
            inp.close_editor(page, true);
        });
    });
    listen(ta, "keydown", |e| {
        e.stop_propagation();
        let Some(k) = e.dyn_ref::<KeyboardEvent>() else { return };
        if let Some(save) = input::editor_key(&k.key(), k.shift_key(), k.is_composing()) {
            e.prevent_default();
            with(|a| {
                let App { inp, page, .. } = a;
                inp.close_editor(page, save);
            });
        }
    });
    if let Some(vv) = w.visual_viewport() {
        listen(&vv, "resize", |_| {
            with(|a| a.page.place_editor());
        });
    }

    // the toolbar, the page arrows and the map
    listen_id("bar", "click", |e| {
        let Some(b) = closest(&e, "button") else { return };
        with(|a| {
            a.bar(&b);
            // leaving the select tool drops the selection; staying keeps its box drawn
            let App { inp, page, .. } = a;
            if page.tool == Tool::Select {
                inp.sel_redraw(page);
            } else {
                inp.deselect(page);
            }
        });
    });
    for (id, di, dj) in [("pgL", -1.0, 0.0), ("pgR", 1.0, 0.0), ("pgU", 0.0, -1.0), ("pgD", 0.0, 1.0)] {
        listen_id(id, "click", move |_| {
            with(|a| a.page_step(di, dj));
        });
    }
    if let Some(nav) = html("nav") {
        let nav_to = |e: &Event| {
            let Some(nav) = html("nav") else { return };
            let r = nav.get_bounding_client_rect();
            let fx = (num(e, "clientX") - r.left()) / r.width();
            let fy = (num(e, "clientY") - r.top()) / r.height();
            with(|a| {
                let changed = a.page.view.nav_to(fx, fy);
                a.page.view_moved(changed, false);
            });
        };
        listen(&nav, "pointerdown", move |e| {
            e.prevent_default();
            if let (Some(nav), Some(p)) = (html("nav"), e.dyn_ref::<PointerEvent>()) {
                let _ = nav.set_pointer_capture(p.pointer_id());
            }
            nav_to(&e);
        });
        listen(&nav, "pointermove", move |e| {
            let captured = match (html("nav"), e.dyn_ref::<PointerEvent>()) {
                (Some(nav), Some(p)) => nav.has_pointer_capture(p.pointer_id()),
                _ => false,
            };
            if captured {
                nav_to(&e);
            }
        });
        for ev in ["pointerup", "pointercancel"] {
            listen(&nav, ev, |_| {
                with(|a| a.page.settle());
            });
        }
    }

    // the toast's Undo
    listen(&page.d.toast, "click", |e| {
        if closest(&e, "button").is_none() {
            return;
        }
        with(|a| {
            a.page.d.toast.set_hidden(true);
            clear_timeout(a.page.toast_timer);
            if let Some(o) = a.page.toast_undo.take() {
                a.page.out(o);
            }
        });
    });

    // panels
    if let Ok(all) = doc().query_selector_all("[data-close]") {
        for i in 0..all.length() {
            if let Some(b) = all.item(i) {
                listen(&b, "click", |_| {
                    with(|a| a.page.open_panel(None));
                });
            }
        }
    }
    listen_id("layerList", "click", |e| {
        let Some(b) = closest(&e, "button[data-act]") else { return };
        let act = b.get_attribute("data-act").unwrap_or_default();
        let Some(l) = b.get_attribute("data-l").and_then(|l| l.parse::<Layer>().ok()) else { return };
        with(|a| {
            let p = &mut a.page;
            match act.as_str() {
                "eye" => {
                    if !p.hidden.shift_remove(&l) {
                        p.hidden.insert(l);
                    }
                    let k = local::hide_key(&p.key);
                    let v = local::hidden_json(&p.hidden);
                    p.ls_set(&k, &v);
                    p.base_redraw(false);
                    p.live_redraw();
                    p.render_layers();
                }
                "ld" | "lr" if p.edit => {
                    if !(p.online && p.ready) {
                        p.show_toast("Not connected. Try again in a moment.", None, 0);
                    } else if act == "ld" {
                        p.out(Out::LayerDelete { l });
                    } else {
                        p.out(Out::LayerRestore { l });
                    }
                }
                _ => {}
            }
        });
    });
    listen_id("shelfList", "click", |e| {
        let Some(b) = closest(&e, "button[data-k]") else { return };
        let Some(k) = b.get_attribute("data-k").and_then(|k| k.parse::<i64>().ok()) else { return };
        with(|a| {
            let App { inp, page, .. } = a;
            if page.not_connected() {
                return;
            }
            if inp.editor.is_some() {
                inp.close_editor(page, true);
            }
            page.out(Out::Uncrumple { k });
            page.open_panel(None);
        });
    });
    listen_id("sharePanel", "click", |e| {
        let Some(b) = closest(&e, "button[data-copy]") else { return };
        let Some(input) = b.get_attribute("data-copy").and_then(|id| by_id::<HtmlInputElement>(&id)) else { return };
        let text = input.value();
        if text.is_empty() {
            return;
        }
        let done = {
            let b = b.clone();
            move || {
                b.set_text_content(Some("Copied"));
                timeout(1500, move || b.set_text_content(Some("Copy")));
            }
        };
        let fallback = {
            let done = done.clone();
            move || {
                let _ = input.focus();
                input.select();
                let ok = doc()
                    .dyn_ref::<web_sys::HtmlDocument>()
                    .is_some_and(|d| d.exec_command("copy").unwrap_or(false));
                if ok {
                    done();
                }
            }
        };
        let clip = Reflect::get(&win().navigator(), &JsValue::from_str("clipboard")).unwrap_or(JsValue::UNDEFINED);
        let write = Reflect::get(&clip, &JsValue::from_str("writeText")).ok().and_then(|f| f.dyn_into::<Function>().ok());
        match write.filter(|_| clip.is_object()) {
            Some(f) => match f.call1(&clip, &JsValue::from_str(&text)) {
                Ok(p) => then(&p, move |_| done(), move |_| fallback()),
                Err(_) => fallback(),
            },
            None => fallback(),
        }
    });
    listen_id("shareNow", "click", |_| {
        with(|a| a.page.ask_share());
    });
    listen_id("shareGo", "click", |_| {
        if with(|a| a.page.answer_share(Answer::Share)) == Some(true) {
            share_now();
        }
    });
    listen_id("shareCancel", "click", |_| {
        with(|a| a.page.answer_share(Answer::Cancel));
    });
    // a click on the dimmed backdrop, outside the box, is a Cancel
    listen_id("shareAsk", "click", |e| {
        if e.target().and_then(|t| t.dyn_into::<Element>().ok()).is_some_and(|t| t.id() == "shareAsk") {
            with(|a| a.page.answer_share(Answer::Cancel));
        }
    });
    listen_id("shareAsk", "keydown", |e| {
        let Some(k) = e.dyn_ref::<KeyboardEvent>() else { return };
        // the window's handler (Esc closes panels, Space pans) never sees the dialog's keys
        e.stop_propagation();
        let btns = ["shareCancel", "shareGo"];
        let focused = doc().active_element().map(|el| el.id());
        let at = btns.iter().position(|b| focused.as_deref() == Some(*b));
        match ask_key(&k.key(), k.shift_key(), at, btns.len()) {
            AskKey::Cancel => {
                e.prevent_default();
                with(|a| a.page.answer_share(Answer::Cancel));
            }
            AskKey::Focus(i) => {
                e.prevent_default();
                if let Some(b) = html(btns[i]) {
                    let _ = b.focus();
                }
            }
            AskKey::None => {}
        }
    });
    listen_id("shapeMenu", "click", |e| {
        with(|a| {
            let App { inp, page, .. } = a;
            inp.cancel_shape(page);
            if closest(&e, "#fillBtn").is_some() {
                page.fill = !page.fill;
                if !matches!(page.tool, Tool::Shape(_)) {
                    page.tool = page.shape_pick;
                }
                page.mark_tools();
                return;
            }
            let Some(b) = closest(&e, "button[data-shape]") else { return };
            let t = match b.get_attribute("data-shape").as_deref() {
                Some("rect") => Tool::Shape(BoxKind::Rect),
                Some("ellipse") => Tool::Shape(BoxKind::Ellipse),
                Some("tri") => Tool::Shape(BoxKind::Tri),
                Some("line") => Tool::Line,
                _ => return,
            };
            page.tool = t;
            page.shape_pick = t;
            page.open_menu(None);
            page.mark_tools();
        });
    });
    listen_id("dashMenu", "click", |e| {
        let Some(b) = closest(&e, "button[data-dash]") else { return };
        let Some(d) = b.get_attribute("data-dash").as_deref().and_then(Dash::from_menu_key) else { return };
        with(|a| {
            a.page.style.d = d;
            a.page.open_menu(None);
            a.page.mark_tools();
        });
    });
    listen_id("gridMenu", "click", |e| {
        let Some(b) = closest(&e, "button[data-grid]") else { return };
        let Some(g) = b.get_attribute("data-grid").as_deref().and_then(Grid::from_key) else { return };
        with(|a| {
            a.page.grid = g;
            a.page.open_menu(None);
            a.page.place_paper();
            a.page.mark_tools();
        });
    });
    listen_id("headMenu", "click", |e| {
        let Some(b) = closest(&e, "button[data-head]") else { return };
        let Some(h) = b.get_attribute("data-head").as_deref().and_then(Heads::from_key) else { return };
        with(|a| {
            a.page.heads = h;
            a.page.open_menu(None);
            a.page.mark_tools();
        });
    });

    // people
    listen(&page.d.who, "click", |_| {
        with(|a| {
            let p = &mut a.page;
            if let Some(n) = p.nudge.take() {
                clear_timeout(p.nudge_timer);
                p.who_dirty = true;
                p.jump_to_ping(n);
                return;
            }
            p.toggle_panel("whoPanel");
        });
    });
    listen_id("whoList", "click", |e| {
        let Some(b) = closest(&e, "button[data-l]") else { return };
        let Some(l) = b.get_attribute("data-l").and_then(|l| l.parse::<Layer>().ok()) else { return };
        with(|a| {
            let p = &mut a.page;
            let Some(ping) = p.last_ping.get(&Some(l)).cloned() else { return };
            p.open_panel(None);
            p.jump_to_ping(ping);
        });
    });
    listen_id("nameIn", "change", |e| {
        let Some(i) = e.target().and_then(|t| t.dyn_into::<HtmlInputElement>().ok()) else { return };
        let n = names::clean_name(&i.value());
        let n = if n.is_empty() { names::roll_name(random(), random()) } else { n };
        with_soon(move |a| a.set_name(n));
    });
    listen_id("reroll", "click", |_| {
        let n = names::roll_name(random(), random());
        with(|a| a.set_name(n));
    });
    listen_id("hueIn", "change", |e| {
        let Some(i) = e.target().and_then(|t| t.dyn_into::<HtmlInputElement>().ok()) else { return };
        let h = hue_of(&i.value());
        with_soon(move |a| {
            a.page.my_hue = Some(h);
            remember("wb.hue", &h.to_string());
            a.page.label_dirty = true;
        });
    });
}

/// Council G6: a panic leaves [`with`]'s borrow of the app held, so every later event is
/// dropped and the page stops without a word. Say so instead, and offer a reload. The hook
/// borrows nothing and cannot panic itself.
fn on_panic(info: &std::panic::PanicHookInfo<'_>) {
    web_sys::console::error_1(&JsValue::from_str(&format!("the page stopped: {info}")));
    let Some(card) = web_sys::window().and_then(|w| w.document()).and_then(|d| d.get_element_by_id("broke")) else {
        return;
    };
    let _ = card.remove_attribute("hidden");
}

/// The page's entry point, called by `boot.js` once the module is loaded.
#[wasm_bindgen]
pub fn start() {
    std::panic::set_hook(Box::new(on_panic));
    let Some(page) = Page::new() else { return };
    wire(&page);
    let mut conn = Conn::default();
    conn.engine = "wasm".into();
    let app = Rc::new(RefCell::new(App { conn, inp: Input::default(), page }));
    APP.with(|a| *a.borrow_mut() = Some(app));
    with(|a| a.boot());
}
