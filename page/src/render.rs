//! Drawing: the two stacked canvases and the time-sliced redraw, from app.js's rendering
//! section.
//!
//! `base` holds finished items and `live` holds strokes in progress. A finished item is stamped
//! onto `base` once, so the cost of drawing stays flat however full the board gets. When the
//! whole of `base` must be redrawn (undo, hide, a delete on a full board), the redraw goes to a
//! spare canvas a few milliseconds per frame and is copied over when it is complete, so the
//! board never flashes empty.
//!
//! Drawing goes through [`Surface`], which the browser's 2D context implements, so the order of
//! the calls is tested without a browser.

use crate::geom::{self, Op};
use crate::model::Live;
use crate::proto::Item;
use crate::view::{View, V};
use crate::shape::{self, Scene};
use std::collections::HashMap;
use web_sys::{CanvasRenderingContext2d, HtmlCanvasElement};

/// Milliseconds a frame spends redrawing finished items (`REDRAW_BUDGET_MS`).
pub const BUDGET_MS: f64 = 6.0;

/// The end angle of a dot's arc. The JavaScript page drew to 6.2832, a hair past 2π; it is kept
/// so the pixels match.
#[allow(clippy::approx_constant)] // deliberately not TAU
pub const DOT_END: f64 = 6.2832;

/// What drawing needs from a 2D context.
pub trait Surface {
    /// `setTransform(a, b, c, d, e, f)`. Not named `set_transform` or `transform`: the context's
    /// own methods of those names would shadow it.
    fn set_matrix(&mut self, m: [f64; 6]);
    /// `wipe`: clear the whole canvas, whatever the transform.
    fn wipe(&mut self);
    /// `prep`: stroke and fill in `c`, round caps and joins.
    fn prep(&mut self, c: &str);
    fn op(&mut self, op: &Op);
    /// `drawText`'s calls: the font and colour, then each line at its point (baseline middle,
    /// aligned left).
    fn text(&mut self, font: &str, c: &str, lines: &[(String, f64, f64)]);
    /// The width in px of `line` in `font`, leaving the context's font as it was.
    fn measure(&mut self, font: &str, line: &str) -> f64;
}

/// Each item's bounding box, worked out once: items never change. JavaScript kept it on the
/// item (`s.bb`); here it is kept by id. An id that comes back (a restored layer, a sheet off
/// the shelf) comes back with the same item, so its box still holds. An item that leaves the
/// board is dropped with [`Bounds::forget`] (the step-9 host calls it on undo, delete and a
/// layer drop); short of that the map is bounded by the board's lifetime, as `dropBoard` clears it.
#[derive(Clone, Debug, Default)]
pub struct Bounds(HashMap<String, [f64; 4]>);

impl Bounds {
    /// `bbox(s)`, measuring text on `ctx`.
    pub fn of(&mut self, s: &Item, ctx: &mut impl Surface) -> [f64; 4] {
        if let Some(b) = self.0.get(&s.id) {
            return *b;
        }
        let font = geom::measure_font(s.st.b, s.st.i);
        let b = geom::bbox(&s.pts, s.w, s.txt.as_deref(), |line| ctx.measure(&font, line));
        self.0.insert(s.id.clone(), b);
        b
    }

    /// Forget one item's box: it left the board (undo, delete, its layer dropped).
    pub fn forget(&mut self, id: &str) {
        self.0.remove(id);
    }

    /// How many boxes are kept.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether no box is kept.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Forget every box (`dropBoard`: a new board).
    pub fn clear(&mut self) {
        self.0.clear();
    }
}

/// `drawFull(ctx, s, v)`: a finished item. With a view `cull`, an item wholly outside it is
/// not drawn.
///
/// A shape or line (A-166) is drawn from `scene`: a line's ends follow the shapes they are
/// attached to, and it hops over the lines below it.
pub fn draw_full(ctx: &mut impl Surface, s: &Item, view: &View, cull: Option<V>, bounds: &mut Bounds, scene: &Scene) {
    if s.sh.is_some() {
        let Some((ops, bb)) = shape::item_ops(s, scene, view.bw) else { return };
        if let Some(v) = cull {
            // a line moves with its shapes, so its box is the scene's, never cached
            let bb = match bb {
                Some(b) => b,
                None => bounds.of(s, ctx),
            };
            if view.off_screen(&bb, v) {
                return;
            }
        }
        ctx.prep(&s.c);
        for op in ops {
            ctx.op(&op);
        }
        return;
    }
    if s.pts.len() < 3 {
        // JS drew nothing visible for a stroke here (an empty path, or NaN widths). A text item
        // with 2 numbers would have drawn at pts[0], pts[1], but the page never makes one.
        return;
    }
    if let Some(v) = cull {
        if view.off_screen(&bounds.of(s, ctx), v) {
            return;
        }
    }
    if let Some(t) = &s.txt {
        let (font, lines) = geom::text_layout(t, &s.pts, s.w, view.bw, s.st.b, s.st.i);
        ctx.text(&font, &s.c, &lines);
        return;
    }
    ctx.prep(&s.c);
    for op in geom::full(&s.pts, s.w, view.bw) {
        ctx.op(&op);
    }
}

/// `drawNew(s)`: the points of a live stroke added since the last call. A hidden layer's
/// stroke is counted as drawn without being drawn.
pub fn draw_new(ctx: &mut impl Surface, s: &mut Live, bw: f64, hidden: bool) {
    let n = s.pts.len() / 3;
    if !hidden {
        ctx.prep(&s.c);
        if s.drawn == 0 && n >= 1 {
            ctx.op(&geom::dot(&s.pts, s.w, bw));
        }
        for i in s.drawn.max(1)..n {
            for op in geom::seg(&s.pts, s.w, bw, i) {
                ctx.op(&op);
            }
        }
    }
    s.drawn = n;
}

/// `redrawLive`, less the pings (the caller draws those after): the live canvas from scratch at
/// the current view.
pub fn redraw_live<'a>(
    ctx: &mut impl Surface,
    view: &View,
    live: impl IntoIterator<Item = &'a mut Live>,
    hidden: impl Fn(&Live) -> bool,
) {
    ctx.set_matrix(view.ctx_transform(view.cur()));
    ctx.wipe();
    for s in live {
        s.drawn = 0;
        let h = hidden(s);
        draw_new(ctx, s, view.bw, h);
    }
}

/// How a slice of the redraw ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// A newer redraw replaced this one: nothing was done.
    Stale,
    /// Out of time: ask for the next frame and call [`Redraw::step`] again with the same `gen`.
    More,
    /// The spare canvas is complete: copy it onto the base ([`blit`]), set the base's
    /// transform to [`Redraw::rv`] and restretch it.
    Swap,
    /// Every item is on the base.
    Done,
}

/// The time-sliced redraw of finished items. The invariant: `strokes[..cursor]` are on the
/// canvas being drawn, which is the spare one while `buffered`.
#[derive(Clone, Debug)]
pub struct Redraw {
    pub gen: u64,
    pub cursor: usize,
    pub buffered: bool,
    /// A frame is asked for (`loopScheduled`).
    pub scheduled: bool,
    /// The view the running redraw draws at.
    pub target: V,
    /// The view the base canvas holds.
    pub rv: V,
}

impl Default for Redraw {
    fn default() -> Redraw {
        let v = V { z: 1.0, x: 0.0, y: 0.0 };
        Redraw { gen: 0, cursor: 0, buffered: false, scheduled: false, target: v, rv: v }
    }
}

impl Redraw {
    /// `redrawBase(direct)`, first half. Returns the new generation. Then the caller, when
    /// `direct`: sets the base's transform to `rv`, restretches and wipes it; otherwise sets the
    /// spare's transform to `target` and wipes it. Then it calls [`Redraw::step`] at once.
    ///
    /// `direct` clears the base and fills it in as it goes (first load, resize).
    pub fn start(&mut self, direct: bool, cur: V) -> u64 {
        self.gen += 1;
        self.scheduled = false;
        self.cursor = 0;
        self.buffered = !direct;
        self.target = cur;
        if direct {
            self.rv = cur;
        }
        self.gen
    }

    /// `step(gen)`: draws items from `cursor` until the budget is spent. `draw(i, buffered)`
    /// draws `strokes[i]` (when its layer is shown) on the spare canvas if `buffered`, else on
    /// the base, at [`Redraw::target`]. `now` is `performance.now()`.
    pub fn step(
        &mut self,
        gen: u64,
        len: usize,
        budget: f64,
        mut now: impl FnMut() -> f64,
        mut draw: impl FnMut(usize, bool),
    ) -> Step {
        if gen != self.gen {
            return Step::Stale;
        }
        self.scheduled = false;
        let t0 = now();
        while self.cursor < len {
            let i = self.cursor;
            self.cursor += 1;
            draw(i, self.buffered);
            if now() - t0 > budget {
                break;
            }
        }
        if self.cursor < len {
            self.scheduled = true;
            Step::More
        } else if self.buffered {
            self.buffered = false;
            self.rv = self.target;
            Step::Swap
        } else {
            Step::Done
        }
    }

    /// `kick`: wake the loop. The generation to step in the next frame, or `None` when a frame
    /// is already asked for.
    pub fn kick(&mut self) -> Option<u64> {
        if self.scheduled {
            return None;
        }
        self.scheduled = true;
        Some(self.gen)
    }
}

impl Surface for CanvasRenderingContext2d {
    fn set_matrix(&mut self, m: [f64; 6]) {
        let _ = CanvasRenderingContext2d::set_transform(self, m[0], m[1], m[2], m[3], m[4], m[5]);
    }

    fn wipe(&mut self) {
        self.save();
        let _ = CanvasRenderingContext2d::set_transform(self, 1.0, 0.0, 0.0, 1.0, 0.0, 0.0);
        if let Some(cv) = self.canvas() {
            self.clear_rect(0.0, 0.0, cv.width() as f64, cv.height() as f64);
        }
        self.restore();
    }

    fn prep(&mut self, c: &str) {
        self.set_stroke_style_str(c);
        self.set_fill_style_str(c);
        self.set_line_cap("round");
        self.set_line_join("round");
    }

    fn op(&mut self, op: &Op) {
        match *op {
            Op::LineWidth(w) => self.set_line_width(w),
            Op::Begin => self.begin_path(),
            Op::MoveTo(x, y) => self.move_to(x, y),
            Op::Quad(a, b, c, d) => self.quadratic_curve_to(a, b, c, d),
            Op::Bezier(a, b, c, d, e, f) => self.bezier_curve_to(a, b, c, d, e, f),
            Op::Stroke => self.stroke(),
            Op::Disc(x, y, r) => {
                self.begin_path();
                let _ = self.arc(x, y, r, 0.0, DOT_END);
                self.fill();
            }
            Op::LineTo(x, y) => self.line_to(x, y),
            Op::Close => self.close_path(),
            Op::Fill => self.fill(),
            Op::Ellipse(x, y, rx, ry) => {
                let _ = self.ellipse(x, y, rx, ry, 0.0, 0.0, std::f64::consts::TAU);
            }
            Op::Arc(x, y, r, a0, a1, ccw) => {
                let _ = self.arc_with_anticlockwise(x, y, r, a0, a1, ccw);
            }
            Op::Alpha(a) => self.set_global_alpha(a),
            Op::Dash(d, g) => {
                let a = js_sys::Array::new();
                if d > 0.0 || g > 0.0 {
                    a.push(&d.into());
                    a.push(&g.into());
                }
                let _ = self.set_line_dash(&a);
            }
            Op::FillColor(c) => {
                self.set_fill_style_str(&format!("#{c:06x}"));
                self.fill();
            }
        }
    }

    fn text(&mut self, font: &str, c: &str, lines: &[(String, f64, f64)]) {
        self.set_font(font);
        self.set_fill_style_str(c);
        self.set_text_baseline("middle");
        self.set_text_align("left");
        for (line, x, y) in lines {
            let _ = self.fill_text(line, *x, *y);
        }
    }

    fn measure(&mut self, font: &str, line: &str) -> f64 {
        self.save();
        self.set_font(font);
        let w = self.measure_text(line).map(|m| m.width()).unwrap_or(0.0);
        self.restore();
        w
    }
}

/// The end of a buffered redraw: the spare canvas, pixel for pixel, onto the wiped base.
pub fn blit(base: &CanvasRenderingContext2d, spare: &HtmlCanvasElement) {
    // a clone of the handle (the same context), only because wipe() takes &mut self
    let mut b = base.clone();
    b.wipe();
    base.save();
    let _ = base.set_transform(1.0, 0.0, 0.0, 1.0, 0.0, 0.0);
    let _ = base.draw_image_with_html_canvas_element(spare, 0.0, 0.0);
    base.restore();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Records the calls, as the differential harness's JavaScript context does.
    #[derive(Default)]
    struct Rec(Vec<String>);

    impl Surface for Rec {
        fn set_matrix(&mut self, m: [f64; 6]) {
            self.0.push(format!("T{m:?}"));
        }
        fn wipe(&mut self) {
            self.0.push("wipe".into());
        }
        fn prep(&mut self, c: &str) {
            self.0.push(format!("prep {c}"));
        }
        fn op(&mut self, op: &Op) {
            self.0.push(format!("{op:?}"));
        }
        fn text(&mut self, font: &str, c: &str, lines: &[(String, f64, f64)]) {
            self.0.push(format!("text {font} {c} {lines:?}"));
        }
        fn measure(&mut self, _font: &str, line: &str) -> f64 {
            self.0.push(format!("measure {line}"));
            line.chars().count() as f64 * 50.0 // 0.5 of the size per character
        }
    }

    fn view() -> View {
        let mut v = View::default();
        v.layout(1600.0, 900.0, 1.0);
        v
    }

    fn stroke(id: &str, pts: Vec<f64>) -> Item {
        Item { id: id.into(), l: Some(1), q: None, c: "#d63031".into(), w: 0.002, pts, txt: None, sh: None, st: Default::default() }
    }

    #[test]
    fn a_stroke_is_prepped_then_drawn_as_geom_says() {
        let v = view();
        let s = stroke("a", vec![0.1, 0.1, 0.5, 0.2, 0.15, 0.5, 0.3, 0.1, 0.5]);
        let mut r = Rec::default();
        draw_full(&mut r, &s, &v, None, &mut Bounds::default(), &Scene::default());
        assert_eq!(r.0[0], "prep #d63031");
        let want: Vec<String> = geom::full(&s.pts, s.w, v.bw).iter().map(|o| format!("{o:?}")).collect();
        assert_eq!(r.0[1..], want[..]);
    }

    #[test]
    fn short_point_lists_draw_nothing() {
        let mut r = Rec::default();
        for pts in [vec![], vec![0.1, 0.2]] {
            draw_full(&mut r, &stroke("a", pts), &view(), None, &mut Bounds::default(), &Scene::default());
        }
        assert!(r.0.is_empty());
    }

    #[test]
    fn off_screen_items_are_culled_and_measured_once() {
        let v = view();
        let cur = v.cur();
        let mut b = Bounds::default();
        let mut r = Rec::default();
        let far = stroke("far", vec![1.9, 1.0, 0.5]);
        draw_full(&mut r, &far, &v, Some(cur), &mut b, &Scene::default());
        assert!(r.0.is_empty(), "outside the view");
        draw_full(&mut r, &far, &v, None, &mut b, &Scene::default());
        assert_eq!(r.0.len(), 2, "no view, no cull: prep and the dot");

        let t = Item { txt: Some("ab\nc".into()), w: 0.02, pts: vec![0.1, 0.1, 0.5], ..stroke("t", vec![]) };
        let mut r = Rec::default();
        draw_full(&mut r, &t, &v, Some(cur), &mut b, &Scene::default());
        draw_full(&mut r, &t, &v, Some(cur), &mut b, &Scene::default());
        let measured = r.0.iter().filter(|c| c.starts_with("measure")).count();
        assert_eq!(measured, 2, "two lines, measured on the first draw only");
        let texts: Vec<&String> = r.0.iter().filter(|c| c.starts_with("text")).collect();
        assert_eq!(texts.len(), 2);
        let (font, lines) = geom::text_layout("ab\nc", &t.pts, t.w, v.bw, t.st.b, t.st.i);
        assert_eq!(*texts[0], format!("text {font} #d63031 {lines:?}"));
        assert_eq!(b.len(), 2, "far and t");
        b.forget("far");
        b.forget("gone");
        assert_eq!(b.len(), 1, "forget drops one box; an unknown id is a no-op");
        let mut r = Rec::default();
        draw_full(&mut r, &t, &v, Some(cur), &mut b, &Scene::default());
        assert!(!r.0.iter().any(|c| c.starts_with("measure")), "t kept its box");
        b.clear();
        assert!(b.is_empty());
    }

    #[test]
    fn two_crossing_connectors_draw_exactly_one_hop() {
        use crate::shape::Shape;
        let sh = |v: serde_json::Value| Shape::from_value(&v);
        let mk = |id: &str, pts: Vec<f64>, v| Item { sh: sh(v), ..stroke(id, pts) };
        let a = mk("A", vec![0.1, 0.1, 0.5, 0.2, 0.2, 0.5], serde_json::json!({"k": "rect", "n": "nA"}));
        let b = mk("B", vec![0.8, 0.1, 0.5, 0.9, 0.2, 0.5], serde_json::json!({"k": "ellipse", "n": "nB"}));
        let c = mk("C", vec![0.1, 0.4, 0.5, 0.2, 0.5, 0.5], serde_json::json!({"k": "tri", "n": "nC"}));
        let d = mk("D", vec![0.8, 0.4, 0.5, 0.9, 0.5, 0.5], serde_json::json!({"k": "rect", "n": "nD", "f": true}));
        // A's right to D's left, and C's right to B's left: an X between the four shapes
        let ad = mk("ad", vec![0.0; 6], serde_json::json!({"k": "line", "a": {"n": "nA", "s": 1, "t": 0.5}, "b": {"n": "nD", "s": 3, "t": 0.5}}));
        let cb = mk("cb", vec![0.0; 6], serde_json::json!({"k": "line", "a": {"n": "nC", "s": 1, "t": 0.5}, "b": {"n": "nB", "s": 3, "t": 0.5}}));
        let items = [a, b, c, d, ad, cb];
        let scene = Scene::build(&items, |_| false);
        let v = view();
        let mut r = Rec::default();
        for it in &items {
            draw_full(&mut r, it, &v, Some(v.cur()), &mut Bounds::default(), &scene);
        }
        assert_eq!(r.0.iter().filter(|c| c.starts_with("Arc")).count(), 1, "{:#?}", r.0);
        assert_eq!(r.0.iter().filter(|c| c.starts_with("Ellipse")).count(), 1);
        assert_eq!(r.0.iter().filter(|c| *c == "Fill").count(), 1 + 2, "the filled box and two heads");
    }

    #[test]
    fn a_live_stroke_draws_only_what_is_new() {
        let mut s = Live { id: "x".into(), l: Some(1), c: "#111111".into(), w: 0.002, pts: vec![0.1, 0.1, 0.5], drawn: 0 };
        let mut r = Rec::default();
        draw_new(&mut r, &mut s, 1000.0, false);
        assert_eq!(r.0, ["prep #111111".to_string(), format!("{:?}", geom::dot(&s.pts, s.w, 1000.0))]);
        s.pts.extend([0.2, 0.1, 0.5, 0.3, 0.2, 0.5]);
        let mut r = Rec::default();
        draw_new(&mut r, &mut s, 1000.0, false);
        let segs: Vec<String> =
            (1..3).flat_map(|i| geom::seg(&s.pts, s.w, 1000.0, i)).map(|o| format!("{o:?}")).collect();
        assert_eq!(r.0[1..], segs[..], "no dot again, the two new pieces");
        assert_eq!(s.drawn, 3);
        s.pts.extend([0.4, 0.2, 0.5]);
        let mut r = Rec::default();
        draw_new(&mut r, &mut s, 1000.0, true);
        assert!(r.0.is_empty());
        assert_eq!(s.drawn, 4, "hidden still counts as drawn");
    }

    #[test]
    fn redraw_live_starts_over_at_the_view() {
        let v = view();
        let mut a = Live { id: "a".into(), l: Some(1), c: "#111111".into(), w: 0.002, pts: vec![0.1, 0.1, 0.5], drawn: 1 };
        let mut b = Live { l: Some(2), ..a.clone() };
        let mut r = Rec::default();
        redraw_live(&mut r, &v, [&mut a, &mut b], |s| s.l == Some(2));
        assert_eq!(r.0[..2], [format!("T{:?}", v.ctx_transform(v.cur())), "wipe".to_string()]);
        assert_eq!(r.0.len(), 4, "a's prep and dot; b hidden");
        assert_eq!((a.drawn, b.drawn), (1, 1));
    }

    /// A clock that moves `per` ms each time it is read.
    fn clock(per: f64) -> impl FnMut() -> f64 {
        let mut t = 0.0;
        move || {
            t += per;
            t
        }
    }

    #[test]
    fn a_direct_redraw_fills_the_base_in_slices() {
        let mut r = Redraw::default();
        let cur = V { z: 2.0, x: 0.1, y: 0.2 };
        let g = r.start(true, cur);
        assert_eq!((r.rv, r.target, r.buffered), (cur, cur, false));
        let mut drawn = vec![];
        // 2.5 ms per item: the budget (more than 6) is passed after the third
        assert_eq!(r.step(g, 7, BUDGET_MS, clock(2.5), |i, b| drawn.push((i, b))), Step::More);
        assert_eq!((r.cursor, r.scheduled), (3, true));
        assert_eq!(r.kick(), None, "a frame is already asked for");
        assert_eq!(r.step(g, 7, BUDGET_MS, clock(2.5), |i, b| drawn.push((i, b))), Step::More);
        assert_eq!(r.step(g, 7, BUDGET_MS, clock(2.5), |i, b| drawn.push((i, b))), Step::Done);
        assert_eq!(drawn, (0..7).map(|i| (i, false)).collect::<Vec<_>>());
        assert!(!r.scheduled);
        assert_eq!(r.kick(), Some(g));
    }

    #[test]
    fn a_buffered_redraw_swaps_at_the_end_and_a_newer_one_wins() {
        let mut r = Redraw::default();
        let old = r.rv;
        let cur = V { z: 1.5, x: 0.0, y: 0.0 };
        let g1 = r.start(false, cur);
        assert_eq!(r.rv, old, "the base keeps its view until the swap");
        assert_eq!(r.step(g1, 4, BUDGET_MS, clock(4.0), |_, b| assert!(b)), Step::More);
        let g2 = r.start(false, cur);
        assert_eq!(r.step(g1, 4, BUDGET_MS, clock(0.0), |_, _| panic!("stale")), Step::Stale);
        assert_eq!(r.cursor, 0);
        assert_eq!(r.step(g2, 4, BUDGET_MS, clock(0.0), |_, b| assert!(b)), Step::Swap);
        assert_eq!((r.rv, r.buffered), (cur, false));
        // an empty board swaps at once
        let g3 = r.start(false, V { z: 1.0, x: 0.0, y: 0.0 });
        assert_eq!(r.step(g3, 0, BUDGET_MS, clock(0.0), |_, _| unreachable!()), Step::Swap);
    }
}
