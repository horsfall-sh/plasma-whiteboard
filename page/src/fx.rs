//! The crumple and unfold animation, from app.js's crumple section (`glInit`, `paperCanvas`,
//! `edge`, `paperOfScreen`, `paperOfStrokes`, `playPaper` and the `fxChain`).
//!
//! The geometry comes from [`wb_crumple::Sheet`], linked in rather than fetched as
//! crumple.wasm: each frame it gives a mesh, drawn here with WebGL using a picture of the board
//! as the paper. Plays run one after another on a chain, as `fxChain = fxChain.then(...)` did.
//! Whatever goes wrong, a play ends, the `#fx` canvas is hidden again and its `done` runs.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast, JsValue};
use wb_crumple::Sheet;
use web_sys::{
    CanvasRenderingContext2d, HtmlCanvasElement, WebGlBuffer, WebGlProgram, WebGlRenderingContext as Gl,
    WebGlUniformLocation,
};

const GRID_COLS: usize = 48;
const GRID_ROWS: usize = 27;
/// The widest paper picture and `#fx` canvas, px.
const MAX_W: u32 = 1600;
/// How long past its length a play may run before it is ended anyway, ms.
const GUARD_MS: f64 = 1500.0;

const VERTEX: &str = "attribute vec4 a; attribute vec2 uv; uniform float asp; varying vec2 v; varying float s;\
void main(){ v = uv; s = a.w; gl_Position = vec4(a.x * 2.0 - 1.0, 1.0 - a.y / asp * 2.0, -a.z * 0.9, 1.0); }";
const FRAGMENT: &str = "precision mediump float; uniform sampler2D tex; varying vec2 v; varying float s;\
void main(){ gl_FragColor = vec4(texture2D(tex, v).rgb * s, 1.0); }";

/// A picture of a sheet of paper to wrap round the mesh. Cheap to clone (a canvas handle).
#[derive(Clone, Debug)]
pub struct Paper {
    pub canvas: HtmlCanvasElement,
}

/// The window, CSS px, as app.js keeps `vw` and `vh`.
fn window_size() -> (f64, f64) {
    let w = web_sys::window().expect("a window");
    let vw = w.inner_width().ok().and_then(|v| v.as_f64()).unwrap_or(1.0).max(1.0);
    let vh = w.inner_height().ok().and_then(|v| v.as_f64()).unwrap_or(1.0).max(1.0);
    (vw, vh)
}

fn document() -> web_sys::Document {
    web_sys::window().expect("a window").document().expect("a document")
}

fn now() -> f64 {
    web_sys::window().and_then(|w| w.performance()).map_or(0.0, |p| p.now())
}

fn clamp(v: f64, lo: f64, hi: f64) -> f64 {
    hi.min(lo.max(v))
}

/// `paperCanvas`: a white canvas as wide as the base canvas (at most [`MAX_W`]) with the
/// window's shape.
fn paper_canvas(base: &HtmlCanvasElement) -> (HtmlCanvasElement, CanvasRenderingContext2d) {
    let (vw, vh) = window_size();
    let c: HtmlCanvasElement = document().create_element("canvas").expect("a canvas").unchecked_into();
    let w = base.width().min(MAX_W);
    c.set_width(w);
    c.set_height(((w as f64 * vh) / vw).round().max(1.0) as u32);
    let ctx: CanvasRenderingContext2d =
        c.get_context("2d").ok().flatten().expect("a 2D context").unchecked_into();
    ctx.set_fill_style_str("#fff");
    ctx.fill_rect(0.0, 0.0, c.width() as f64, c.height() as f64);
    (c, ctx)
}

/// `edge`: the sheet's grey border.
fn edge(c: HtmlCanvasElement, ctx: &CanvasRenderingContext2d) -> Paper {
    let _ = ctx.set_transform(1.0, 0.0, 0.0, 1.0, 0.0, 0.0);
    ctx.set_stroke_style_str("#c9ced6");
    ctx.set_line_width(3.0);
    ctx.stroke_rect(0.0, 0.0, c.width() as f64, c.height() as f64);
    Paper { canvas: c }
}

struct GlState {
    gl: Gl,
    pos: WebGlBuffer,
    n: usize,
    count: i32,
    asp: Option<WebGlUniformLocation>,
}

/// Raw bytes of a slice of plain numbers, for the WebGL upload calls.
fn bytes<T: Copy>(v: &[T]) -> &[u8] {
    // SAFETY: f32 and u16 have no padding and every byte pattern is a valid u8.
    unsafe { std::slice::from_raw_parts(v.as_ptr() as *const u8, std::mem::size_of_val(v)) }
}

fn gl_init(fx: &HtmlCanvasElement) -> Option<GlState> {
    let opts = js_sys::Object::new();
    for k in ["alpha", "antialias", "depth"] {
        js_sys::Reflect::set(&opts, &JsValue::from_str(k), &JsValue::TRUE).ok()?;
    }
    let gl: Gl = fx.get_context_with_context_options("webgl", &opts).ok()??.dyn_into().ok()?;
    let sh = |ty: u32, src: &str| {
        let o = gl.create_shader(ty)?;
        gl.shader_source(&o, src);
        gl.compile_shader(&o);
        Some(o)
    };
    let prog: WebGlProgram = gl.create_program()?;
    gl.attach_shader(&prog, &sh(Gl::VERTEX_SHADER, VERTEX)?);
    gl.attach_shader(&prog, &sh(Gl::FRAGMENT_SHADER, FRAGMENT)?);
    gl.link_program(&prog);
    if !gl.get_program_parameter(&prog, Gl::LINK_STATUS).as_bool().unwrap_or(false) {
        return None;
    }
    gl.use_program(Some(&prog));
    let asp = gl.get_uniform_location(&prog, "asp");
    let (c, r) = (GRID_COLS, GRID_ROWS);
    let n = (c + 1) * (r + 1);
    let mut uv = vec![0f32; n * 2];
    for j in 0..=r {
        for i in 0..=c {
            uv[(j * (c + 1) + i) * 2] = i as f32 / c as f32;
            uv[(j * (c + 1) + i) * 2 + 1] = j as f32 / r as f32;
        }
    }
    let mut idx: Vec<u16> = Vec::with_capacity(c * r * 6);
    for j in 0..r {
        for i in 0..c {
            let a = (j * (c + 1) + i) as u16;
            let (b, cc) = (a + 1, a + c as u16 + 1);
            let d = cc + 1;
            idx.extend_from_slice(&[a, b, cc, b, d, cc]);
        }
    }
    let attr = |name: &str, size: i32, data: Option<&[u8]>, len: i32, usage: u32| {
        let buf = gl.create_buffer()?;
        let loc = gl.get_attrib_location(&prog, name);
        if loc < 0 {
            return None;
        }
        gl.bind_buffer(Gl::ARRAY_BUFFER, Some(&buf));
        match data {
            Some(d) => gl.buffer_data_with_u8_array(Gl::ARRAY_BUFFER, d, usage),
            None => gl.buffer_data_with_i32(Gl::ARRAY_BUFFER, len, usage),
        }
        gl.enable_vertex_attrib_array(loc as u32);
        gl.vertex_attrib_pointer_with_i32(loc as u32, size, Gl::FLOAT, false, 0, 0);
        Some(buf)
    };
    attr("uv", 2, Some(bytes(&uv)), 0, Gl::STATIC_DRAW)?;
    let pos = attr("a", 4, None, (n * 4 * 4) as i32, Gl::DYNAMIC_DRAW)?;
    gl.bind_buffer(Gl::ELEMENT_ARRAY_BUFFER, gl.create_buffer().as_ref());
    gl.buffer_data_with_u8_array(Gl::ELEMENT_ARRAY_BUFFER, bytes(&idx), Gl::STATIC_DRAW);
    gl.bind_texture(Gl::TEXTURE_2D, gl.create_texture().as_ref());
    gl.tex_parameteri(Gl::TEXTURE_2D, Gl::TEXTURE_WRAP_S, Gl::CLAMP_TO_EDGE as i32);
    gl.tex_parameteri(Gl::TEXTURE_2D, Gl::TEXTURE_WRAP_T, Gl::CLAMP_TO_EDGE as i32);
    gl.tex_parameteri(Gl::TEXTURE_2D, Gl::TEXTURE_MIN_FILTER, Gl::LINEAR as i32);
    gl.tex_parameteri(Gl::TEXTURE_2D, Gl::TEXTURE_MAG_FILTER, Gl::LINEAR as i32);
    gl.enable(Gl::DEPTH_TEST);
    gl.clear_color(0.0, 0.0, 0.0, 0.0);
    Some(GlState { gl, pos, n, count: idx.len() as i32, asp })
}

struct Play {
    tex: Paper,
    reverse: bool,
    ms: f64,
    done: Option<Box<dyn FnOnce()>>,
}

/// One play in progress.
struct Running {
    /// Which play this is: a frame or guard of an earlier play does nothing to it.
    id: u64,
    done: Option<Box<dyn FnOnce()>>,
    guard: i32,
    over: bool,
}

struct Inner {
    fx: HtmlCanvasElement,
    /// `prefers-reduced-motion: reduce` when the page loaded.
    calm: bool,
    /// None: not tried yet; Some(None): no WebGL here.
    gl: Option<Option<GlState>>,
    queue: VecDeque<Play>,
    running: Option<Running>,
    sheet: Option<Sheet>,
    plays: u64,
}

/// The effect canvas and its chain of plays. Clones share one chain.
#[derive(Clone)]
pub struct Fx(Rc<RefCell<Inner>>);

impl Fx {
    /// WebGL is set up on the first play that can run, as app.js's `glInit` was; without it,
    /// plays end at once.
    pub fn new(fx_canvas: HtmlCanvasElement) -> Fx {
        let calm = web_sys::window()
            .and_then(|w| w.match_media("(prefers-reduced-motion: reduce)").ok().flatten())
            .is_some_and(|m| m.matches());
        Fx(Rc::new(RefCell::new(Inner {
            fx: fx_canvas,
            calm,
            gl: None,
            queue: VecDeque::new(),
            running: None,
            sheet: None,
            plays: 0,
        })))
    }

    /// `paperOfScreen` when the base canvas is sharp: a picture of what `base` and `live` show.
    /// The caller checks first, as app.js did, that the base is drawn at the current view and
    /// complete (`rv` is the view and `cursor == strokes.len()`), and otherwise calls
    /// [`Fx::paper_of_strokes`].
    pub fn paper_of_screen(&self, base: &HtmlCanvasElement, live: &HtmlCanvasElement) -> Paper {
        let (c, ctx) = paper_canvas(base);
        let (w, h) = (c.width() as f64, c.height() as f64);
        for cv in [base, live] {
            let _ = ctx.draw_image_with_html_canvas_element_and_dw_and_dh(cv, 0.0, 0.0, w, h);
        }
        edge(c, &ctx)
    }

    /// `paperOfStrokes`: what the window would show, drawn in one go. `draw` gets the paper's
    /// 2D context and its width in px, and draws the board's visible items onto it: app.js set
    /// `k = width / vw * zoom`, then `setTransform(k, 0, 0, k, -panX * bw * k, -panY * bw * k)`,
    /// then `drawFull` for each item not on a hidden layer, and drew nothing for a board of more
    /// than 1500 items (plain paper). The edge is drawn after `draw`, with the transform reset.
    pub fn paper_of_strokes(&self, base: &HtmlCanvasElement, draw: impl FnOnce(&CanvasRenderingContext2d, f64)) -> Paper {
        let (c, ctx) = paper_canvas(base);
        draw(&ctx, c.width() as f64);
        edge(c, &ctx)
    }

    /// `fxChain = fxChain.then(() => playPaper(tex, reverse, ms))`: queue a play of the paper
    /// crumpling (reversed, unfolding) over `ms`. `done` runs when this play ends, at once if
    /// the animation cannot run here (no WebGL, reduced motion, a hidden page, an empty paper).
    pub fn play(&mut self, tex: Paper, reverse: bool, ms: f64, done: Option<Box<dyn FnOnce()>>) {
        self.0.borrow_mut().queue.push_back(Play { tex, reverse, ms, done });
        if self.0.borrow().running.is_none() {
            self.next();
        }
    }

    /// Start the next queued play, if none is running.
    fn next(&self) {
        let play = {
            let mut s = self.0.borrow_mut();
            if s.running.is_some() {
                return;
            }
            match s.queue.pop_front() {
                Some(p) => p,
                None => return,
            }
        };
        let id = {
            let mut s = self.0.borrow_mut();
            s.plays += 1;
            s.plays
        };
        let w = web_sys::window().expect("a window");
        let me = self.clone();
        let guard_cb = Closure::once_into_js(move || me.finish(id));
        let guard = w
            .set_timeout_with_callback_and_timeout_and_arguments_0(guard_cb.unchecked_ref(), (play.ms + GUARD_MS) as i32)
            .unwrap_or(0);
        self.0.borrow_mut().running = Some(Running { id, done: play.done, guard, over: false });
        if !self.begin(id, &play.tex, play.reverse, play.ms) {
            self.finish(id);
        }
    }

    /// The setup half of `playPaper`. False: the animation cannot run, end the play now.
    fn begin(&self, id: u64, tex: &Paper, reverse: bool, ms: f64) -> bool {
        let hidden_page = document().hidden();
        let (vw, vh) = window_size();
        let base_w = document()
            .get_element_by_id("base")
            .and_then(|e| e.dyn_into::<HtmlCanvasElement>().ok())
            .map_or(0, |b| b.width());
        let a = vh / vw;
        {
            let mut s = self.0.borrow_mut();
            if s.calm || hidden_page || tex.canvas.width() == 0 {
                return false;
            }
            if s.gl.is_none() {
                let g = gl_init(&s.fx);
                s.gl = Some(g);
            }
            let Some(Some(g)) = s.gl.as_ref() else { return false };
            let gl = g.gl.clone();
            let asp = g.asp.clone();
            let fx = s.fx.clone();
            fx.set_width(base_w.min(MAX_W));
            fx.set_height((fx.width() as f64 * a).round().max(1.0) as u32);
            gl.viewport(0, 0, fx.width() as i32, fx.height() as i32);
            gl.uniform1f(asp.as_ref(), a as f32);
            if gl
                .tex_image_2d_with_u32_and_u32_and_canvas(
                    Gl::TEXTURE_2D,
                    0,
                    Gl::RGBA as i32,
                    Gl::RGBA,
                    Gl::UNSIGNED_BYTE,
                    &tex.canvas,
                )
                .is_err()
            {
                return false;
            }
            let seed = (js_sys::Math::random() * 0x7fff_ffff as f64) as u32;
            let mut sheet = Sheet::new(GRID_COLS, GRID_ROWS, seed, a as f32);
            // The wad flies to the Shelf button.
            let (tx, ty) = match document().get_element_by_id("shelfBtn") {
                Some(b) => {
                    let r = b.get_bounding_client_rect();
                    if r.width() != 0.0 {
                        ((r.left() + r.width() / 2.0) / vw, (r.top() + r.height() / 2.0) / vw)
                    } else {
                        (0.06, 0.05)
                    }
                }
                None => (0.06, 0.05),
            };
            sheet.set_target(clamp(tx, -0.05, 1.05) as f32, clamp(ty, -0.05, a + 0.05) as f32);
            s.sheet = Some(sheet);
            fx.set_hidden(false);
        }
        let t0 = now();
        self.tick(id, t0, reverse, ms);
        true
    }

    /// Ask for the next frame of the play started at `t0`.
    fn tick(&self, id: u64, t0: f64, reverse: bool, ms: f64) {
        let me = self.clone();
        let cb = Closure::once_into_js(move |now: f64| me.frame(id, now, t0, reverse, ms));
        let asked = web_sys::window().map(|w| w.request_animation_frame(cb.unchecked_ref()));
        if !matches!(asked, Some(Ok(_))) {
            self.finish(id);
        }
    }

    fn frame(&self, id: u64, now: f64, t0: f64, reverse: bool, ms: f64) {
        let p = {
            let mut s = self.0.borrow_mut();
            if s.running.as_ref().is_none_or(|r| r.over || r.id != id) {
                return;
            }
            let p = ((now - t0) / ms).clamp(0.0, 1.0);
            let p = if p.is_nan() { 1.0 } else { p };
            let Inner { gl, sheet, .. } = &mut *s;
            let (Some(Some(g)), Some(sheet)) = (gl.as_ref(), sheet.as_mut()) else {
                drop(s);
                self.finish(id);
                return;
            };
            sheet.frame((if reverse { 1.0 - p } else { p }) as f32);
            let data = sheet.data();
            let data = &data[..data.len().min(g.n * 4)];
            g.gl.bind_buffer(Gl::ARRAY_BUFFER, Some(&g.pos));
            g.gl.buffer_sub_data_with_i32_and_u8_array(Gl::ARRAY_BUFFER, 0, bytes(data));
            g.gl.clear(Gl::COLOR_BUFFER_BIT | Gl::DEPTH_BUFFER_BIT);
            g.gl.draw_elements_with_i32(Gl::TRIANGLES, g.count, Gl::UNSIGNED_SHORT, 0);
            p
        };
        if p < 1.0 {
            self.tick(id, t0, reverse, ms);
        } else {
            self.finish(id);
        }
    }

    /// `finish`: once per play. Hide and shrink the canvas, run `done`, start the next play.
    fn finish(&self, id: u64) {
        let done = {
            let mut s = self.0.borrow_mut();
            let Some(r) = s.running.as_mut() else { return };
            if r.over || r.id != id {
                return;
            }
            r.over = true;
            let guard = r.guard;
            let done = r.done.take();
            s.running = None;
            s.sheet = None;
            if let Some(w) = web_sys::window() {
                w.clear_timeout_with_handle(guard);
            }
            s.fx.set_hidden(true);
            s.fx.set_width(1);
            s.fx.set_height(1);
            done
        };
        if let Some(d) = done {
            d();
        }
        self.next();
    }
}
