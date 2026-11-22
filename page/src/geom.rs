//! Shapes: line widths, bounding boxes, the curves strokes are drawn along, the finger
//! smoothing, colours and pings. Nothing here touches a canvas: a stroke's outline comes back
//! as a list of [`Op`]s for the render module to replay, so the drawing rules of app.js
//! (`dot`, `seg`, `drawFull`, `drawText`, `bbox`, `curveThrough`, `steady`, `hueHex`,
//! `onPing`) are tested on the host.

use crate::view::js_round;

/// The simplification tolerance: about a third of a pixel on a 1300 px page.
pub const EPS: f64 = 0.00025;
pub const ERASER_SCALE: f64 = 6.0;
pub const ERASER_COLOR: &str = "#ffffff";
pub const FIXED_COLORS: [&str; 4] = ["#111111", "#d63031", "#0a66d8", "#1e9e4a"];
/// Font size of a text item = its width times this.
pub const TEXT_SCALE: f64 = 5.0;
pub const LINE_HEIGHT: f64 = 1.25;
pub const FONT: &str = "-apple-system, \"Segoe UI\", Roboto, Helvetica, Arial, sans-serif";
/// Turns sharper than about 70 degrees are corners.
pub const CORNER_COS: f64 = 0.35;
/// A ping with no layer colour.
pub const PING_COLOR: &str = "#e67e22";

/// The name of a fixed colour, for its button.
pub fn color_name(c: &str) -> Option<&'static str> {
    match c {
        "#111111" => Some("Black"),
        "#d63031" => Some("Red"),
        "#0a66d8" => Some("Blue"),
        "#1e9e4a" => Some("Green"),
        _ => None,
    }
}

/// `r5`: round to 5 decimals so points stay short on the wire.
pub fn r5(v: f64) -> f64 {
    js_round(v * 1e5) / 1e5
}

/// `simplify`: Ramer-Douglas-Peucker through smooth/ (in f32, as the page passed it), then
/// each value rounded with `r5`. Two points or fewer come back unchanged.
pub fn simplify(pts: &[f64], eps: f64) -> Vec<f64> {
    let n = pts.len() / 3;
    if n <= 2 {
        return pts.to_vec();
    }
    let mut buf: Vec<f32> = pts[..n * 3].iter().map(|&v| v as f32).collect();
    let m = wb_smooth::simplify(&mut buf, eps as f32);
    buf[..m * 3].iter().map(|&v| r5(v as f64)).collect()
}

/// `lineW`: a stroke's width in px at pressure `p`, board width `bw`.
pub fn line_w(w: f64, bw: f64, p: f64) -> f64 {
    0.6f64.max(w * bw * (0.4 + 1.2 * p))
}

/// One drawing step on a 2D context, in board px (board units times `bw`).
#[derive(Clone, Debug, PartialEq)]
pub enum Op {
    LineWidth(f64),
    Begin,
    MoveTo(f64, f64),
    Quad(f64, f64, f64, f64),
    Bezier(f64, f64, f64, f64, f64, f64),
    Stroke,
    /// A filled disc: centre and radius.
    Disc(f64, f64, f64),
}

/// `dot`: a one-point stroke.
pub fn dot(pts: &[f64], w: f64, bw: f64) -> Op {
    Op::Disc(pts[0] * bw, pts[1] * bw, line_w(w, bw, pts[2]) / 2.0)
}

/// `seg`: the live-canvas piece that ends at the midpoint between points i-1 and i.
pub fn seg(pts: &[f64], w: f64, bw: f64, i: usize) -> Vec<Op> {
    let (j, k) = ((i - 1) * 3, i * 3);
    let (x1, y1, x2, y2) = (pts[j] * bw, pts[j + 1] * bw, pts[k] * bw, pts[k + 1] * bw);
    let mut ops = vec![Op::LineWidth(line_w(w, bw, (pts[j + 2] + pts[k + 2]) / 2.0)), Op::Begin];
    if i == 1 {
        ops.push(Op::MoveTo(x1, y1));
    } else {
        ops.push(Op::MoveTo((pts[j - 3] * bw + x1) / 2.0, (pts[j - 2] * bw + y1) / 2.0));
    }
    ops.push(Op::Quad(x1, y1, (x1 + x2) / 2.0, (y1 + y2) / 2.0));
    ops.push(Op::Stroke);
    ops
}

/// `drawFull` for a stroke (not text): a smooth curve through the points, each piece a cubic
/// whose tangents follow the neighbouring points (Catmull-Rom), held to a third of the shorter
/// neighbouring piece; sharp turns stay corners. Pieces of the same width share one path.
pub fn full(pts: &[f64], w: f64, bw: f64) -> Vec<Op> {
    let n = pts.len() / 3;
    if n == 0 {
        return vec![];
    }
    if n == 1 {
        return vec![dot(pts, w, bw)];
    }
    let p = pts;
    let mut ops = Vec::with_capacity(n + 4);
    let mut lw = line_w(w, bw, (p[2] + p[5]) / 2.0);
    ops.push(Op::LineWidth(lw));
    let (mut x1, mut y1) = (p[0] * bw, p[1] * bw);
    ops.push(Op::Begin);
    ops.push(Op::MoveTo(x1, y1));
    let (mut ux, mut uy, mut len_in) = (0.0f64, 0.0f64, 0.0f64);
    for i in 0..n - 1 {
        let k = i * 3;
        let (x2, y2) = (p[k + 3] * bw, p[k + 4] * bw);
        let len = hypot(x2 - x1, y2 - y1);
        let (mut vx, mut vy, mut len_out) = (0.0, 0.0, 0.0);
        if i < n - 2 && len > 0.0 {
            let (x3, y3) = (p[k + 6] * bw, p[k + 7] * bw);
            len_out = hypot(x3 - x2, y3 - y2);
            if len_out > 0.0 {
                let cos = ((x2 - x1) * (x3 - x2) + (y2 - y1) * (y3 - y2)) / (len * len_out);
                if cos > CORNER_COS {
                    let d = or1(hypot(x3 - x1, y3 - y1));
                    vx = (x3 - x1) / d;
                    vy = (y3 - y1) / d;
                }
            }
        }
        let wi = line_w(w, bw, (p[k + 2] + p[k + 5]) / 2.0);
        if wi != lw {
            ops.push(Op::Stroke);
            lw = wi;
            ops.push(Op::LineWidth(lw));
            ops.push(Op::Begin);
            ops.push(Op::MoveTo(x1, y1));
        }
        let a = len.min(or_else(len_in, len)) / 3.0;
        let b = len.min(or_else(len_out, len)) / 3.0;
        ops.push(Op::Bezier(x1 + ux * a, y1 + uy * a, x2 - vx * b, y2 - vy * b, x2, y2));
        x1 = x2;
        y1 = y2;
        ux = vx;
        uy = vy;
        len_in = len;
    }
    ops.push(Op::Stroke);
    ops
}

/// `Math.hypot` of two numbers as V8 works it out (scale by the larger, Kahan-sum the
/// squares), which can differ in the last bit from libm's `hypot`. Points that a finger's
/// line is re-sampled at go on the wire, so they must be the same doubles.
pub fn hypot(a: f64, b: f64) -> f64 {
    let (a, b) = (a.abs(), b.abs());
    if a.is_infinite() || b.is_infinite() {
        return f64::INFINITY;
    }
    if a.is_nan() || b.is_nan() {
        return f64::NAN;
    }
    let max = a.max(b);
    if max == 0.0 {
        return 0.0;
    }
    let (mut sum, mut comp) = (0.0f64, 0.0f64);
    for v in [a, b] {
        let r = v / max;
        let s = r * r - comp;
        let p = sum + s;
        comp = (p - sum) - s;
        sum = p;
    }
    sum.sqrt() * max
}

/// JavaScript `x || 1` for a number.
fn or1(x: f64) -> f64 {
    or_else(x, 1.0)
}

/// JavaScript `x || y` for numbers: 0 and NaN are falsy.
fn or_else(x: f64, y: f64) -> f64 {
    if x == 0.0 || x.is_nan() {
        y
    } else {
        x
    }
}

/// `textLines`.
pub fn text_lines(txt: &str) -> std::str::Split<'_, char> {
    txt.split('\n')
}

/// `drawText`: the font, and each line's baseline point (textBaseline middle, align left).
pub fn text_layout(txt: &str, pts: &[f64], w: f64, bw: f64) -> (String, Vec<(String, f64, f64)>) {
    let px = w * bw;
    let lh = px * LINE_HEIGHT;
    let font = format!("{}px {}", crate::json::num_to_string(px), FONT);
    let lines = text_lines(txt)
        .enumerate()
        .map(|(i, line)| (line.to_string(), pts[0] * bw, pts[1] * bw + (i as f64 + 0.5) * lh))
        .collect();
    (font, lines)
}

/// The font `bbox` measures text in: 100 px, then scaled.
pub fn measure_font() -> String {
    format!("100px {}", FONT)
}

/// `bbox`, in board units [x0, y0, x1, y1]. A text item's width comes from `measure`, the
/// width in px of a line drawn in [`measure_font`].
pub fn bbox(pts: &[f64], w: f64, txt: Option<&str>, mut measure: impl FnMut(&str) -> f64) -> [f64; 4] {
    let (mut x0, mut x1, mut y0, mut y1) = (pts[0], pts[0], pts[1], pts[1]);
    if let Some(t) = txt {
        let px = 100.0;
        let mut wmax = 0.0f64;
        let mut lines = 0usize;
        for line in text_lines(t) {
            wmax = wmax.max(measure(line));
            lines += 1;
        }
        x1 = x0 + (wmax / px) * w;
        y1 = y0 + lines as f64 * LINE_HEIGHT * w;
    } else {
        let mut k = 3;
        while k + 1 < pts.len() {
            if pts[k] < x0 {
                x0 = pts[k];
            } else if pts[k] > x1 {
                x1 = pts[k];
            }
            if pts[k + 1] < y0 {
                y0 = pts[k + 1];
            } else if pts[k + 1] > y1 {
                y1 = pts[k + 1];
            }
            k += 3;
        }
        x0 -= w;
        y0 -= w;
        x1 += w;
        y1 += w;
    }
    [x0, y0, x1, y1]
}

/// `curveThrough`: where samples are far apart (in screen px, `px` per board unit), fill in
/// along a centripetal Catmull-Rom spline. Pressure becomes 0.5 throughout.
pub fn curve_through(pts: &[f64], px: f64) -> Vec<f64> {
    let mut q: Vec<f64> = Vec::new();
    let mut i = 0;
    while i + 1 < pts.len() {
        let n = q.len();
        if n == 0 || q[n - 2] != pts[i] || q[n - 1] != pts[i + 1] {
            q.push(pts[i]);
            q.push(pts[i + 1]);
        }
        i += 3;
    }
    let m = (q.len() / 2) as isize;
    if m < 3 {
        return pts.to_vec();
    }
    let xf = |i: isize| -> f64 {
        if i < 0 {
            2.0 * q[0] - q[2]
        } else if i >= m {
            2.0 * q[(2 * m - 2) as usize] - q[(2 * m - 4) as usize]
        } else {
            q[(2 * i) as usize]
        }
    };
    let yf = |i: isize| -> f64 {
        if i < 0 {
            2.0 * q[1] - q[3]
        } else if i >= m {
            2.0 * q[(2 * m - 1) as usize] - q[(2 * m - 3) as usize]
        } else {
            q[(2 * i + 1) as usize]
        }
    };
    let mut out = vec![q[0], q[1], 0.5];
    for i in 0..m - 1 {
        let (x1, y1, x2, y2) = (xf(i), yf(i), xf(i + 1), yf(i + 1));
        let steps = 64.0f64.min((hypot(x2 - x1, y2 - y1) * px / 2.0).ceil());
        if steps > 2.0 {
            let (x0, y0, x3, y3) = (xf(i - 1), yf(i - 1), xf(i + 2), yf(i + 2));
            let t1 = or_else(hypot(x1 - x0, y1 - y0).sqrt(), 1e-6);
            let t2 = t1 + hypot(x2 - x1, y2 - y1).sqrt();
            let t3 = t2 + or_else(hypot(x3 - x2, y3 - y2).sqrt(), 1e-6);
            let mut k = 1.0;
            while k < steps {
                let t = t1 + ((t2 - t1) * k) / steps;
                let a1x = ((t1 - t) * x0 + t * x1) / t1;
                let a1y = ((t1 - t) * y0 + t * y1) / t1;
                let a2x = ((t2 - t) * x1 + (t - t1) * x2) / (t2 - t1);
                let a2y = ((t2 - t) * y1 + (t - t1) * y2) / (t2 - t1);
                let a3x = ((t3 - t) * x2 + (t - t2) * x3) / (t3 - t2);
                let a3y = ((t3 - t) * y2 + (t - t2) * y3) / (t3 - t2);
                let b1x = ((t2 - t) * a1x + t * a2x) / t2;
                let b1y = ((t2 - t) * a1y + t * a2y) / t2;
                let b2x = ((t3 - t) * a2x + (t - t1) * a3x) / (t3 - t1);
                let b2y = ((t3 - t) * a2y + (t - t1) * a3y) / (t3 - t1);
                out.push(((t2 - t) * b1x + (t - t1) * b2x) / (t2 - t1));
                out.push(((t2 - t) * b1y + (t - t1) * b2y) / (t2 - t1));
                out.push(0.5);
                k += 1.0;
            }
        }
        out.extend_from_slice(&[x2, y2, 0.5]);
    }
    out
}

/// exp(-(1.5k)^2 / (2 * 3.5^2)), k = 0..7.
const K: [f64; 8] = [1.0, 0.912, 0.693, 0.438, 0.23, 0.101, 0.037, 0.011];

/// `steady`: a finger's line, re-sampled every 1.5 screen px and averaged both ways with a
/// bell-shaped weighting. The ends stay where they were.
pub fn steady(pts: &[f64], px: f64) -> Vec<f64> {
    if pts.len() < 9 {
        return pts.to_vec();
    }
    let pts = curve_through(pts, px);
    let n = pts.len() / 3;
    let step = 1.5 / px;
    let mut r: Vec<f64> = Vec::new();
    let (mut x, mut y, mut left) = (pts[0], pts[1], step);
    r.push(x);
    r.push(y);
    for i in 1..n {
        let (tx, ty) = (pts[i * 3], pts[i * 3 + 1]);
        let mut d = hypot(tx - x, ty - y);
        while d >= left {
            x += ((tx - x) * left) / d;
            y += ((ty - y) * left) / d;
            d -= left;
            left = step;
            r.push(x);
            r.push(y);
        }
        left -= d;
        x = tx;
        y = ty;
    }
    r.push(x);
    r.push(y);
    let m = r.len() / 2;
    if !(5..=60000).contains(&m) {
        return pts;
    }
    let mut out = Vec::with_capacity(m * 3);
    for i in 0..m {
        let rr = i.min(m - 1 - i).min(K.len() - 1) as isize;
        let (mut sx, mut sy, mut sw) = (0.0, 0.0, 0.0);
        for k in -rr..=rr {
            let w = K[k.unsigned_abs()];
            let j = (i as isize + k) as usize;
            sx += r[j * 2] * w;
            sy += r[j * 2 + 1] * w;
            sw += w;
        }
        out.extend_from_slice(&[r5(sx / sw), r5(sy / sw), 0.5]);
    }
    out
}

/// `hueHex`: hsl(h, 70%, 42%) as #rrggbb.
pub fn hue_hex(h: f64) -> String {
    let (s, l) = (0.7, 0.42);
    let c = (1.0 - (2.0 * l - 1.0f64).abs()) * s;
    let x = c * (1.0 - (((h / 60.0) % 2.0) - 1.0).abs());
    let m = l - c / 2.0;
    let sextant = ((h / 60.0).floor() as i64).rem_euclid(6);
    let (r, g, b) = match sextant {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let hex = |v: f64| format!("{:02x}", js_round((v + m) * 255.0) as i64);
    format!("#{}{}{}", hex(r), hex(g), hex(b))
}

/// The colour check `onName` applies: `#` and six hex digits, any case.
pub fn is_hex_color(c: &str) -> bool {
    let b = c.as_bytes();
    b.len() == 7 && b[0] == b'#' && b[1..].iter().all(u8::is_ascii_hexdigit)
}

/// `onPing`'s box [x0, y0, x1, y1] round a ping's points.
pub fn ping_box(pts: &[f64]) -> [f64; 4] {
    let mut bb = [f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY];
    let mut i = 0;
    while i + 1 < pts.len() {
        bb[0] = bb[0].min(pts[i]);
        bb[1] = bb[1].min(pts[i + 1]);
        bb[2] = bb[2].max(pts[i]);
        bb[3] = bb[3].max(pts[i + 1]);
        i += 3;
    }
    bb
}

pub const PING_MS: f64 = 6000.0;

/// `drawPings`: a ping's opacity and pulse at `age` ms.
pub fn ping_glow(age: f64) -> (f64, f64) {
    let a = (age / 200.0).min(1.0) * ((PING_MS - age) / 1500.0).min(1.0);
    let pulse = 0.65 + 0.35 * (age / 170.0).sin();
    (a, pulse)
}

/// `sendPing`'s point budget: simplify (after `steady` for a finger), doubling the tolerance
/// until 200 points or fewer remain. None: too short to ping.
pub fn ping_points(pts: &[f64], touch: bool, px: f64) -> Option<Vec<f64>> {
    if pts.len() < 6 {
        return None;
    }
    let mut eps = 2.0 / px;
    let src = if touch { steady(pts, px) } else { pts.to_vec() };
    let mut p = simplify(&src, eps);
    while p.len() > 600 {
        eps *= 2.0;
        p = simplify(&p, eps);
    }
    Some(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn r5_rounds_as_the_page_did() {
        assert_eq!(r5(0.123456), 0.12346);
        assert_eq!(r5(-0.000005), -0.0); // Math.round(-0.5) is -0
        assert_eq!(r5(0.000005), 0.00001);
        assert_eq!(r5(0.1f32 as f64), 0.1);
    }

    #[test]
    fn simplify_keeps_short_strokes_and_rounds_f32_output() {
        assert_eq!(simplify(&[0.1, 0.2, 0.5, 0.3, 0.4, 0.5], EPS), vec![0.1, 0.2, 0.5, 0.3, 0.4, 0.5]);
        // a straight line collapses to its ends
        let line: Vec<f64> = (0..10).flat_map(|i| [i as f64 * 0.01, 0.0, 0.5]).collect();
        assert_eq!(simplify(&line, EPS), vec![0.0, 0.0, 0.5, 0.09, 0.0, 0.5]);
    }

    #[test]
    fn hypot_is_v8s() {
        // Math.hypot in Node (V8); each differs from sqrt(a*a + b*b) in the last bit.
        assert_eq!(hypot(36.98935508728027, 41.53265953063965), 55.61631233245278);
        assert_eq!(hypot(-42.54403114318848, 49.73731338977814), 65.4507061011995);
        assert_eq!(hypot(11.811867356300354, -0.4041433334350586), 11.818779221086835);
        assert_eq!(hypot(0.0, -0.0), 0.0);
        assert_eq!(hypot(f64::NAN, f64::INFINITY), f64::INFINITY);
        assert!(hypot(f64::NAN, 1.0).is_nan());
    }

    #[test]
    fn line_width_has_a_floor() {
        assert_eq!(line_w(0.004, 1000.0, 0.5), 0.004 * 1000.0 * 1.0);
        assert_eq!(line_w(0.0001, 100.0, 0.0), 0.6);
    }

    #[test]
    fn one_point_is_a_dot() {
        assert_eq!(full(&[0.1, 0.2, 0.5], 0.004, 1000.0), vec![Op::Disc(100.0, 200.0, 2.0)]);
        assert!(full(&[], 0.004, 1000.0).is_empty());
    }

    #[test]
    fn two_points_are_one_straight_cubic() {
        let ops = full(&[0.0, 0.0, 0.5, 0.3, 0.0, 0.5], 0.004, 1000.0);
        assert_eq!(
            ops,
            vec![
                Op::LineWidth(4.0),
                Op::Begin,
                Op::MoveTo(0.0, 0.0),
                Op::Bezier(0.0, 0.0, 300.0, 0.0, 300.0, 0.0),
                Op::Stroke
            ]
        );
    }

    #[test]
    fn a_gentle_turn_is_smooth_and_a_sharp_one_is_a_corner() {
        // gentle: the tangent at the middle point follows the chord from first to last
        let ops = full(&[0.0, 0.0, 0.5, 0.1, 0.0, 0.5, 0.2, 0.01, 0.5], 0.004, 1000.0);
        let Op::Bezier(_, _, c2x, c2y, _, _) = ops[3] else { panic!() };
        assert!(c2x < 100.0 && c2y < 0.0);
        // sharp: straight back, so a zero tangent at the corner
        let ops = full(&[0.0, 0.0, 0.5, 0.1, 0.0, 0.5, 0.0, 0.0, 0.5], 0.004, 1000.0);
        assert_eq!(ops[3], Op::Bezier(0.0, 0.0, 100.0, 0.0, 100.0, 0.0));
    }

    #[test]
    fn a_change_of_width_starts_a_new_path() {
        let ops = full(&[0.0, 0.0, 0.5, 0.1, 0.0, 0.5, 0.2, 0.0, 1.0], 0.004, 1000.0);
        let strokes = ops.iter().filter(|o| **o == Op::Stroke).count();
        assert_eq!(strokes, 2);
        assert!(ops.contains(&Op::MoveTo(100.0, 0.0)));
    }

    #[test]
    fn seg_starts_at_the_previous_midpoint() {
        let p = [0.0, 0.0, 0.5, 0.1, 0.0, 0.5, 0.2, 0.1, 0.5];
        assert_eq!(seg(&p, 0.004, 1000.0, 1)[2], Op::MoveTo(0.0, 0.0));
        let s = seg(&p, 0.004, 1000.0, 2);
        assert_eq!(s[2], Op::MoveTo(50.0, 0.0));
        assert_eq!(s[3], Op::Quad(100.0, 0.0, 150.0, 50.0));
    }

    #[test]
    fn text_lines_are_spaced_by_line_height() {
        let (font, lines) = text_layout("a\nbc", &[0.1, 0.2, 0.5], 0.02, 1000.0);
        assert_eq!(font, format!("20px {}", FONT));
        assert_eq!(lines[0], ("a".to_string(), 100.0, 200.0 + 12.5));
        assert_eq!(lines[1], ("bc".to_string(), 100.0, 200.0 + 37.5));
    }

    #[test]
    fn bbox_pads_strokes_and_measures_text() {
        assert_eq!(bbox(&[0.1, 0.2, 0.5, 0.3, 0.1, 0.5], 0.01, None, |_| 0.0), [0.1 - 0.01, 0.1 - 0.01, 0.3 + 0.01, 0.2 + 0.01]);
        let b = bbox(&[0.1, 0.2, 0.5], 0.02, Some("ab\nabcd"), |s| s.len() as f64 * 50.0);
        assert_eq!(b, [0.1, 0.2, 0.1 + 2.0 * 0.02, 0.2 + 2.0 * 1.25 * 0.02]);
    }

    #[test]
    fn curve_through_fills_in_far_samples_only() {
        let near = [0.0, 0.0, 0.7, 0.001, 0.0, 0.7, 0.002, 0.0, 0.7];
        let out = curve_through(&near, 1000.0);
        assert_eq!(out, vec![0.0, 0.0, 0.5, 0.001, 0.0, 0.5, 0.002, 0.0, 0.5]);
        let far = [0.0, 0.0, 0.5, 0.1, 0.0, 0.5, 0.2, 0.0, 0.5];
        let out = curve_through(&far, 1000.0);
        // 100 px apart: 50 steps per piece
        assert_eq!(out.len() / 3, 1 + 2 * 50);
        assert_eq!(&out[out.len() - 3..], &[0.2, 0.0, 0.5]);
        // a repeated point is dropped, and fewer than three points come back as they were
        assert_eq!(curve_through(&[0.0, 0.0, 0.3, 0.0, 0.0, 0.3, 1.0, 1.0, 0.3], 1000.0).len(), 9);
    }

    #[test]
    fn steady_keeps_the_ends_and_flattens_a_wobble() {
        let mut pts = vec![];
        for i in 0..40 {
            let wob = if i % 2 == 0 { 0.0005 } else { -0.0005 };
            pts.extend_from_slice(&[i as f64 * 0.002, if i == 0 || i == 39 { 0.0 } else { wob }, 0.5]);
        }
        let out = steady(&pts, 1000.0);
        assert_eq!(&out[..2], &[0.0, 0.0]);
        assert_eq!(&out[out.len() - 3..out.len() - 1], &[0.078, 0.0]);
        let worst = out.chunks(3).map(|p| p[1].abs()).fold(0.0, f64::max);
        assert!(worst < 0.0003, "{worst}");
        assert_eq!(steady(&[0.0, 0.0, 0.5, 1.0, 1.0, 0.5], 1000.0), vec![0.0, 0.0, 0.5, 1.0, 1.0, 0.5]);
    }

    #[test]
    fn hue_hex_matches_the_page() {
        // values worked out from app.js hueHex
        assert_eq!(hue_hex(0.0), "#b62020");
        assert_eq!(hue_hex(120.0), "#20b620");
        assert_eq!(hue_hex(210.0), "#206bb6");
        assert_eq!(hue_hex(359.0), "#b62023");
        assert!(is_hex_color("#A0b1c2") && !is_hex_color("#a0b1c") && !is_hex_color("a0b1c2f"));
    }

    #[test]
    fn pings() {
        assert_eq!(ping_box(&[0.1, 0.5, 1.0, -0.2, 0.3, 1.0]), [-0.2, 0.3, 0.1, 0.5]);
        assert_eq!(ping_glow(0.0).0, 0.0);
        assert_eq!(ping_glow(1000.0).0, 1.0);
        assert!(ping_glow(5999.0).0 < 0.001);
        assert!(ping_points(&[0.0, 0.0, 0.5], false, 1000.0).is_none());
        let ring: Vec<f64> = (0..2000)
            .flat_map(|i| {
                let a = i as f64 * 0.0031;
                [0.1 * a.cos(), 0.1 * a.sin(), 0.5]
            })
            .collect();
        assert!(ping_points(&ring, false, 1000.0).unwrap().len() <= 600);
        assert_eq!(color_name("#0a66d8"), Some("Blue"));
    }
}
