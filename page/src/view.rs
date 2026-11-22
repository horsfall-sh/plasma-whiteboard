//! The view: how much of the board is on screen and where. This is the zoom and pan half of
//! app.js (`visW` .. `boardPoint`, `layout`), with the side effects left to the caller: each
//! change returns whether the view moved, and the web side then places the paper, stretches the
//! base canvas, redraws the live canvas and so on, as app.js did.
//!
//! The board is nine pages, three by three, each 1 unit wide and `ASPECT` tall. Board units are
//! page widths; the middle page's top left is (0, 0).

/// Page height over page width.
pub const ASPECT: f64 = 9.0 / 16.0;
/// The whole surface: left, top, width, height.
pub const WX: f64 = -1.0;
pub const WY: f64 = -ASPECT;
pub const WW: f64 = 3.0;
pub const WH: f64 = 3.0 * ASPECT;
/// At 100% a page shows with a little of its neighbours.
pub const OVERLAP: f64 = 1.08;
pub const MAX_ZOOM: f64 = 8.0;

/// `clamp`.
pub fn clamp(v: f64, lo: f64, hi: f64) -> f64 {
    hi.min(lo.max(v))
}

/// `clampPan`: keep the visible span `vis` inside [lo, lo + size], or centre it when it is
/// wider than that.
pub fn clamp_pan(p: f64, vis: f64, lo: f64, size: f64) -> f64 {
    if vis >= size {
        lo + (size - vis) / 2.0
    } else {
        (lo + size - vis).min(lo.max(p))
    }
}

/// A view: `z` is the zoom, (`x`, `y`) the board point at the window's top left.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct V {
    pub z: f64,
    pub x: f64,
    pub y: f64,
}

/// A CSS box in px, or in percent for the map.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub left: f64,
    pub top: f64,
    pub width: f64,
    pub height: f64,
}

/// Which page arrows are hidden, and the map box (None: the map is hidden).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Chrome {
    /// `Math.round(zoom * 100)`, for the zoom label.
    pub percent: f64,
    pub hide_l: bool,
    pub hide_r: bool,
    pub hide_u: bool,
    pub hide_d: bool,
    pub nav: Option<Rect>,
}

/// The window and the view on it.
#[derive(Clone, Debug, PartialEq)]
pub struct View {
    /// The window, CSS px.
    pub vw: f64,
    pub vh: f64,
    /// The board's width in CSS px at 100%.
    pub bw: f64,
    pub zoom: f64,
    pub pan_x: f64,
    pub pan_y: f64,
    /// Device pixels per CSS px, capped at 2.
    pub dpr: f64,
    laid_out: bool,
}

impl Default for View {
    fn default() -> Self {
        View { vw: 1.0, vh: 1.0, bw: 1.0, zoom: 1.0, pan_x: 0.0, pan_y: 0.0, dpr: 1.0, laid_out: false }
    }
}

impl View {
    pub fn cur(&self) -> V {
        V { z: self.zoom, x: self.pan_x, y: self.pan_y }
    }
    /// Board units across the window at zoom `z`.
    pub fn vis_w(&self, z: f64) -> f64 {
        self.vw / (self.bw * z)
    }
    pub fn vis_h(&self, z: f64) -> f64 {
        self.vh / (self.bw * z)
    }
    /// The zoom at which every page fits.
    pub fn min_zoom(&self) -> f64 {
        (self.vw / WW).min(self.vh / WH) / self.bw
    }

    /// `layout`: the window is now `inner_w` by `inner_h` CSS px at `device_pixel_ratio`.
    /// Keeps the same board point at the middle of the window. Returns the canvas size in
    /// device px.
    pub fn layout(&mut self, inner_w: f64, inner_h: f64, device_pixel_ratio: f64) -> (u32, u32) {
        let (mut cx, cy) = if self.laid_out {
            (self.pan_x + self.vis_w(self.zoom) / 2.0, self.pan_y + self.vis_h(self.zoom) / 2.0)
        } else {
            (0.5, ASPECT / 2.0)
        };
        self.vw = inner_w.max(1.0);
        self.vh = inner_h.max(1.0);
        self.bw = self.vw.max(self.vh / ASPECT) / OVERLAP;
        // Start at 100%. On a phone held upright that shows about a third of the page's width;
        // start at the page's left edge, where writing starts.
        if !self.laid_out {
            self.zoom = 1.0;
            if self.vis_w(1.0) < 0.7 {
                cx = self.vis_w(1.0) / 2.0 - 0.02;
            }
        }
        self.laid_out = true;
        self.zoom = MAX_ZOOM.min(self.min_zoom().max(self.zoom));
        self.pan_x = clamp_pan(cx - self.vis_w(self.zoom) / 2.0, self.vis_w(self.zoom), WX, WW);
        self.pan_y = clamp_pan(cy - self.vis_h(self.zoom) / 2.0, self.vis_h(self.zoom), WY, WH);
        let dpr = if device_pixel_ratio > 0.0 { device_pixel_ratio } else { 1.0 };
        self.dpr = dpr.min(2.0);
        (js_round(self.vw * self.dpr) as u32, js_round(self.vh * self.dpr) as u32)
    }

    /// `setView` without its side effects: clamp and store the view, and say whether it moved.
    pub fn set(&mut self, z: f64, x: f64, y: f64) -> bool {
        let mut z = MAX_ZOOM.min(self.min_zoom().max(z));
        if (z - 1.0).abs() < 0.02 {
            z = 1.0;
        }
        let x = clamp_pan(x, self.vis_w(z), WX, WW);
        let y = clamp_pan(y, self.vis_h(z), WY, WH);
        let same = z == self.zoom && x == self.pan_x && y == self.pan_y;
        self.zoom = z;
        self.pan_x = x;
        self.pan_y = y;
        !same
    }

    /// `zoomAt`: zoom by `f` keeping the screen point (sx, sy) over the same board point.
    pub fn zoom_at(&mut self, f: f64, sx: f64, sy: f64) -> bool {
        let z = MAX_ZOOM.min(self.min_zoom().max(self.zoom * f));
        let k = self.bw * self.zoom;
        let bx = self.pan_x + sx / k;
        let by = self.pan_y + sy / k;
        self.set(z, bx - sx / (self.bw * z), by - sy / (self.bw * z))
    }

    /// `zoomStep`: zoom about the middle of the window.
    pub fn zoom_step(&mut self, f: f64) -> bool {
        self.zoom_at(f, self.vw / 2.0, self.vh / 2.0)
    }

    /// `zoomHome`: 100% -> the whole page (where the window is too narrow to show one at
    /// 100%) -> all nine -> 100%.
    pub fn zoom_home(&mut self) -> bool {
        let pf = self.vw.min(self.vh / ASPECT) / self.bw;
        let (i, j) = self.page_at();
        if self.zoom == 1.0 && pf < 0.9 {
            let (x, y) = (i + 0.5 - self.vis_w(pf) / 2.0, (j + 0.5) * ASPECT - self.vis_h(pf) / 2.0);
            self.set(pf, x, y)
        } else if self.zoom == 1.0 || (self.zoom < 1.0 && self.zoom > self.min_zoom() * 1.02) {
            self.set(self.min_zoom(), WX, WY)
        } else {
            self.go_page(i, j)
        }
    }

    /// `pageAt`: the page under the middle of the window, (column, row), each -1, 0 or 1.
    pub fn page_at(&self) -> (f64, f64) {
        (
            clamp((self.pan_x + self.vis_w(self.zoom) / 2.0).floor(), -1.0, 1.0),
            clamp(((self.pan_y + self.vis_h(self.zoom) / 2.0) / ASPECT).floor(), -1.0, 1.0),
        )
    }

    /// `goPage`: that page at 100%, centred.
    pub fn go_page(&mut self, i: f64, j: f64) -> bool {
        let i = clamp(i, -1.0, 1.0);
        let j = clamp(j, -1.0, 1.0);
        self.set(1.0, i + 0.5 - self.vis_w(1.0) / 2.0, (j + 0.5) * ASPECT - self.vis_h(1.0) / 2.0)
    }

    /// `pageStep`: the arrows move a page at a time, or most of a screenful where a whole page
    /// does not fit at this zoom.
    pub fn page_step(&mut self, di: f64, dj: f64) -> bool {
        let w = self.vis_w(self.zoom);
        let h = self.vis_h(self.zoom);
        if di != 0.0 && w < 0.9 {
            return self.set(self.zoom, self.pan_x + di * w * 0.85, self.pan_y);
        }
        if dj != 0.0 && h < ASPECT * 0.9 {
            return self.set(self.zoom, self.pan_x, self.pan_y + dj * h * 0.85);
        }
        let (i, j) = self.page_at();
        self.go_page(i + di, j + dj)
    }

    /// `navTo`: put the map point (fx, fy), each 0..1 across the map, at the centre.
    pub fn nav_to(&mut self, fx: f64, fy: f64) -> bool {
        let bx = WX + fx * WW;
        let by = WY + fy * WH;
        self.set(self.zoom, bx - self.vis_w(self.zoom) / 2.0, by - self.vis_h(self.zoom) / 2.0)
    }

    /// Centre the board point (cx, cy), at this zoom (`jumpToPing`).
    pub fn centre_on(&mut self, cx: f64, cy: f64) -> bool {
        self.set(self.zoom, cx - self.vis_w(self.zoom) / 2.0, cy - self.vis_h(self.zoom) / 2.0)
    }

    /// `showZoom`: the zoom label, the arrows and the map.
    pub fn chrome(&self) -> Chrome {
        let w = self.vis_w(self.zoom);
        let h = self.vis_h(self.zoom);
        let all = w >= WW - 1e-6 && h >= WH - 1e-6;
        let e = 1e-4;
        let nav = if all {
            None
        } else {
            Some(Rect {
                left: ((WX.max(self.pan_x) - WX) / WW) * 100.0,
                top: ((WY.max(self.pan_y) - WY) / WH) * 100.0,
                width: (WW.min(w) / WW) * 100.0,
                height: (WH.min(h) / WH) * 100.0,
            })
        };
        Chrome {
            percent: js_round(self.zoom * 100.0),
            hide_l: all || self.pan_x <= WX + e,
            hide_r: all || self.pan_x + w >= WX + WW - e,
            hide_u: all || self.pan_y <= WY + e,
            hide_d: all || self.pan_y + h >= WY + WH - e,
            nav,
        }
    }

    /// `placePaper`: the paper element's box, CSS px.
    pub fn paper(&self) -> Rect {
        let k = self.bw * self.zoom;
        Rect { left: (WX - self.pan_x) * k, top: (WY - self.pan_y) * k, width: WW * k, height: WH * k }
    }

    /// `setCtxView`: the canvas transform (a, b, c, d, e, f) that draws board px (board units
    /// times `bw`) at view `v`.
    pub fn ctx_transform(&self, v: V) -> [f64; 6] {
        let k = self.dpr * v.z;
        [k, 0.0, 0.0, k, -v.x * self.bw * k, -v.y * self.bw * k]
    }

    /// `stretchBase`: the CSS transform that shows the base canvas, drawn at `rv`, at the
    /// current view. Empty when they are the same.
    pub fn stretch(&self, rv: V) -> String {
        if rv.z == self.zoom && rv.x == self.pan_x && rv.y == self.pan_y {
            return String::new();
        }
        let f = self.zoom / rv.z;
        let k = self.bw * self.zoom;
        format!(
            "translate({}px, {}px) scale({})",
            crate::json::num_to_string((rv.x - self.pan_x) * k),
            crate::json::num_to_string((rv.y - self.pan_y) * k),
            crate::json::num_to_string(f)
        )
    }

    /// `boardPoint`: the board point under the pointer at (dx, dy) CSS px from the board's top
    /// left, clamped to the surface.
    pub fn board_point(&self, dx: f64, dy: f64) -> (f64, f64) {
        let k = self.bw * self.zoom;
        (clamp(self.pan_x + dx / k, WX, WX + WW), clamp(self.pan_y + dy / k, WY, WY + WH))
    }

    /// `offScreen`: the box [x0, y0, x1, y1] (board units) is wholly outside view `v`.
    pub fn off_screen(&self, b: &[f64; 4], v: V) -> bool {
        b[2] < v.x || b[3] < v.y || b[0] > v.x + self.vis_w(v.z) || b[1] > v.y + self.vis_h(v.z)
    }

    /// `penW`: a pen of nominal width `w` is the same thickness on screen at any zoom.
    pub fn pen_w(&self, w: f64) -> f64 {
        0.05f64.min(0.0004f64.max(crate::geom::r5((w * 1000.0) / (self.bw * self.zoom))))
    }
}

/// JavaScript's `Math.round`: halves go up, toward +infinity.
pub fn js_round(x: f64) -> f64 {
    if !x.is_finite() {
        return x;
    }
    let f = x.floor();
    let r = if x - f >= 0.5 { f + 1.0 } else { f };
    if r == 0.0 && x < 0.0 {
        -0.0
    } else {
        r
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    fn laid(w: f64, h: f64) -> View {
        let mut v = View::default();
        v.layout(w, h, 1.0);
        v
    }

    #[test]
    fn js_round_matches_math_round() {
        assert_eq!(js_round(2.5), 3.0);
        assert_eq!(js_round(-2.5), -2.0);
        assert_eq!(js_round(0.49999999999999994), 0.0);
        assert_eq!(js_round(-0.2).to_bits(), (-0.0f64).to_bits());
        assert_eq!(js_round(1.4999), 1.0);
    }

    #[test]
    fn clamp_pan_centres_a_span_wider_than_the_surface() {
        assert_eq!(clamp_pan(5.0, 4.0, -1.0, 3.0), -1.5);
        assert_eq!(clamp_pan(5.0, 1.0, -1.0, 3.0), 1.0);
        assert_eq!(clamp_pan(-5.0, 1.0, -1.0, 3.0), -1.0);
        assert_eq!(clamp_pan(0.3, 1.0, -1.0, 3.0), 0.3);
    }

    #[test]
    fn a_landscape_window_starts_on_the_middle_page_at_100() {
        let v = laid(1600.0, 900.0);
        assert_eq!(v.bw, 1600.0 / OVERLAP);
        assert_eq!(v.zoom, 1.0);
        // the middle page's centre is the window's centre
        assert!(close(v.pan_x + v.vis_w(1.0) / 2.0, 0.5));
        assert!(close(v.pan_y + v.vis_h(1.0) / 2.0, ASPECT / 2.0));
        assert_eq!(v.page_at(), (0.0, 0.0));
    }

    #[test]
    fn an_upright_phone_starts_at_the_page_left_edge() {
        let v = laid(400.0, 800.0);
        assert!(v.vis_w(1.0) < 0.7);
        // cx = visW/2 - 0.02, so pan_x = -0.02
        assert!(close(v.pan_x, -0.02));
    }

    #[test]
    fn layout_keeps_the_middle_point_on_resize() {
        let mut v = laid(1600.0, 900.0);
        v.set(2.0, 0.2, 0.1);
        let (cx, cy) = (v.pan_x + v.vis_w(2.0) / 2.0, v.pan_y + v.vis_h(2.0) / 2.0);
        v.layout(1200.0, 900.0, 3.0);
        assert!(close(v.pan_x + v.vis_w(v.zoom) / 2.0, cx));
        assert!(close(v.pan_y + v.vis_h(v.zoom) / 2.0, cy));
        assert_eq!(v.dpr, 2.0);
    }

    #[test]
    fn layout_returns_the_canvas_size_in_device_px() {
        let mut v = View::default();
        assert_eq!(v.layout(1001.0, 500.5, 1.5), (1502, 751));
        assert_eq!(View::default().layout(0.0, 0.0, 0.0), (1, 1));
    }

    #[test]
    fn set_snaps_near_100_and_clamps_zoom() {
        let mut v = laid(1600.0, 900.0);
        assert!(v.set(1.015, 0.0, 0.0));
        assert_eq!(v.zoom, 1.0);
        v.set(100.0, 0.0, 0.0);
        assert_eq!(v.zoom, MAX_ZOOM);
        v.set(0.0001, 0.0, 0.0);
        assert_eq!(v.zoom, v.min_zoom());
        // all nine fit: the pan is centred
        assert!(close(v.pan_x, WX + (WW - v.vis_w(v.zoom)) / 2.0));
        assert!(!v.set(v.zoom, v.pan_x, v.pan_y));
    }

    #[test]
    fn zoom_at_keeps_the_point_under_the_pointer() {
        let mut v = laid(1600.0, 900.0);
        let (sx, sy) = (400.0, 300.0);
        let before = v.board_point(sx, sy);
        v.zoom_at(2.0, sx, sy);
        let after = v.board_point(sx, sy);
        assert!(close(before.0, after.0) && close(before.1, after.1));
        assert_eq!(v.zoom, 2.0);
    }

    #[test]
    fn zoom_home_cycles_100_all_100() {
        let mut v = laid(1600.0, 900.0);
        // pf >= 0.9 on a landscape window: 100% -> all nine
        v.zoom_home();
        assert_eq!(v.zoom, v.min_zoom());
        // all nine -> the page under the middle at 100%
        v.zoom_home();
        assert_eq!(v.zoom, 1.0);
        assert_eq!(v.page_at(), (0.0, 0.0));
    }

    #[test]
    fn zoom_home_on_an_upright_phone_shows_the_whole_page_first() {
        let mut v = laid(400.0, 800.0);
        v.zoom_home();
        let pf = 400.0f64.min(800.0 / ASPECT) / v.bw;
        assert!(close(v.zoom, pf));
        assert!(close(v.pan_x + v.vis_w(pf) / 2.0, 0.5));
    }

    #[test]
    fn page_step_moves_a_page_or_a_screenful() {
        let mut v = laid(1600.0, 900.0);
        v.page_step(1.0, 0.0);
        assert_eq!(v.page_at(), (1.0, 0.0));
        v.page_step(1.0, 0.0); // already at the edge column
        assert_eq!(v.page_at(), (1.0, 0.0));
        v.page_step(0.0, -1.0);
        assert_eq!(v.page_at(), (1.0, -1.0));
        let mut p = laid(400.0, 800.0);
        let (x0, w) = (p.pan_x, p.vis_w(1.0));
        p.page_step(1.0, 0.0);
        assert!(close(p.pan_x, x0 + w * 0.85));
    }

    #[test]
    fn chrome_hides_arrows_at_the_edges_and_the_map_when_all_shows() {
        let mut v = laid(1600.0, 900.0);
        let c = v.chrome();
        assert_eq!(c.percent, 100.0);
        assert!(!c.hide_l && !c.hide_r && !c.hide_u && !c.hide_d);
        let n = c.nav.unwrap();
        assert!(close(n.width, v.vis_w(1.0) / WW * 100.0));
        v.go_page(-1.0, -1.0);
        let c = v.chrome();
        assert!(c.hide_l && c.hide_u && !c.hide_r && !c.hide_d);
        assert_eq!(c.nav.unwrap().left, 0.0);
        v.set(v.min_zoom(), WX, WY);
        let c = v.chrome();
        assert!(c.hide_l && c.hide_r && c.hide_u && c.hide_d && c.nav.is_none());
    }

    #[test]
    fn nav_to_centres_the_map_point() {
        let mut v = laid(1600.0, 900.0);
        v.set(2.0, 0.0, 0.0);
        v.nav_to(0.5, 0.5);
        assert!(close(v.pan_x + v.vis_w(2.0) / 2.0, WX + WW / 2.0));
        assert!(close(v.pan_y + v.vis_h(2.0) / 2.0, WY + WH / 2.0));
    }

    #[test]
    fn paper_transform_and_stretch() {
        let mut v = laid(1080.0, 600.0);
        assert!(close(v.bw, 1000.0));
        v.bw = 1000.0;
        v.set(2.0, 0.0, 0.0);
        let p = v.paper();
        assert_eq!((p.left, p.width), (-2000.0, 6000.0));
        v.dpr = 2.0;
        assert_eq!(v.ctx_transform(V { z: 2.0, x: 0.1, y: 0.0 }), [4.0, 0.0, 0.0, 4.0, -400.0, -0.0]);
        assert_eq!(v.stretch(v.cur()), "");
        assert_eq!(v.stretch(V { z: 1.0, x: 0.1, y: 0.05 }), "translate(200px, 100px) scale(2)");
    }

    #[test]
    fn board_point_clamps_to_the_surface() {
        let v = laid(1600.0, 900.0);
        let (x, _) = v.board_point(-1e9, 0.0);
        assert_eq!(x, WX);
        let (_, y) = v.board_point(0.0, 1e9);
        assert_eq!(y, WY + WH);
    }

    #[test]
    fn off_screen_and_pen_width() {
        let v = laid(1080.0, 600.0);
        let cur = v.cur();
        assert!(!v.off_screen(&[0.1, 0.1, 0.2, 0.2], cur));
        assert!(v.off_screen(&[1.5, 0.1, 1.6, 0.2], cur));
        assert_eq!(v.pen_w(0.004), 0.004);
        assert_eq!(v.pen_w(1.0), 0.05);
        assert_eq!(v.pen_w(0.0), 0.0004);
    }
}
