//! Shapes and lines (A-166): squares, circles and triangles placed by dragging a box, and
//! straight lines whose ends can attach to a shape's anchors.
//!
//! On the wire a shape or line is an ordinary `s` item with a `sh` object (PROTOCOL.md):
//!
//! * a shape: `{"k":"rect"|"ellipse"|"tri","n":<node>,"f":<filled>}`, `pts` = the two corners
//!   of its box. `n` is the shape's **node name**, which stays the same when the shape moves
//!   (a move is an undo of the old item plus a new item with the same `n`).
//! * a line: `{"k":"line","h":"none"|"end"|"start"|"both","a"?:<end>,"b"?:<end>}`, `pts` =
//!   the two ends as drawn. An attached end is `{"n":<node>,"s":<side>,"t":<0..1>}`: side `s`
//!   of the shape and `t` along it, both relative to the shape's box, so the end follows the
//!   shape through a move or a resize. A free end (absent) is the drawn point in `pts`.
//!
//! Everything here is plain geometry over board units; [`Op`]s come out in board px.

use crate::geom::{line_w, Op};
use crate::json::J;
use crate::proto::Item;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::f64::consts::{FRAC_PI_2, PI};

/// Sub-anchors on an expanded side sit at `k / SUBS` for `k` in `1..SUBS`: 15 per side.
pub const SUBS: u32 = 16;
/// How long the pointer rests on a side's anchor before moving off it expands the side.
pub const DWELL_MS: f64 = 1600.0;
/// How near (screen px) a line end must come to a shape for its anchors to show.
pub const NEAR_PX: f64 = 24.0;
/// How near (screen px) to an anchor a line end snaps to it.
pub const SNAP_PX: f64 = 12.0;
/// Sub-anchors are closer together, so they snap from less far.
pub const SUB_SNAP_PX: f64 = 6.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BoxKind {
    Rect,
    Ellipse,
    Tri,
}

impl BoxKind {
    pub fn key(self) -> &'static str {
        match self {
            BoxKind::Rect => "rect",
            BoxKind::Ellipse => "ellipse",
            BoxKind::Tri => "tri",
        }
    }

    pub fn from_key(k: &str) -> Option<BoxKind> {
        Some(match k {
            "rect" => BoxKind::Rect,
            "ellipse" => BoxKind::Ellipse,
            "tri" => BoxKind::Tri,
            _ => return None,
        })
    }

    /// How many sides carry anchors: a triangle has three.
    pub fn sides(self) -> u8 {
        if self == BoxKind::Tri {
            3
        } else {
            4
        }
    }
}

/// Which ends of a line carry an arrowhead.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Heads {
    None,
    #[default]
    End,
    Start,
    Both,
}

impl Heads {
    pub fn key(self) -> &'static str {
        match self {
            Heads::None => "none",
            Heads::End => "end",
            Heads::Start => "start",
            Heads::Both => "both",
        }
    }

    pub fn from_key(k: &str) -> Option<Heads> {
        Some(match k {
            "none" => Heads::None,
            "end" => Heads::End,
            "start" => Heads::Start,
            "both" => Heads::Both,
            _ => return None,
        })
    }

    pub fn at_start(self) -> bool {
        matches!(self, Heads::Start | Heads::Both)
    }

    pub fn at_end(self) -> bool {
        matches!(self, Heads::End | Heads::Both)
    }
}

/// Where a line end is attached: side `s` of node `n`, `t` along that side.
#[derive(Clone, Debug, PartialEq)]
pub struct Anchor {
    pub n: String,
    pub s: u8,
    pub t: f64,
}

impl Anchor {
    fn from_value(v: &Value) -> Option<Anchor> {
        let s = v.get("s")?.as_u64().filter(|s| *s <= 3)? as u8;
        let t = v.get("t")?.as_f64().filter(|t| t.is_finite())?.clamp(0.0, 1.0);
        Some(Anchor { n: v.get("n")?.as_str()?.to_string(), s, t })
    }

    fn to_json(&self) -> J {
        J::obj([("n", J::str(&self.n)), ("s", J::Num(self.s as f64)), ("t", J::Num(self.t))])
    }

    /// The same place on the shape, by value (sub-anchors are kept to 1/SUBS steps, so this
    /// is exact in practice).
    pub fn same(&self, o: &Anchor) -> bool {
        self.n == o.n && self.s == o.s && (self.t - o.t).abs() < 1e-6
    }
}

/// How a line is drawn (`sh.d`): dashes and gaps keep a fixed length in canvas units at any
/// thickness, and dots stay round.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Dash {
    #[default]
    Solid,
    Dash,
    Dot,
}

/// A dash and the gap after it, board units, as they look on the board.
pub const DASH_LEN: f64 = 0.014;
pub const DASH_GAP: f64 = 0.008;
/// The gap between dots, board units.
pub const DOT_GAP: f64 = 0.006;

impl Dash {
    /// The `setLineDash` pattern (board px) for a stroke `lw` board px thick with round caps:
    /// a cap adds lw/2 at each end of a dash, so the pattern takes it back off the dash and
    /// gives it to the gap. What shows is a DASH_LEN dash (never shorter than a dot) and a
    /// DASH_GAP gap at any thickness; a dot is a zero-length dash, a round cap of diameter lw.
    pub fn pattern(self, lw: f64, bw: f64) -> Option<(f64, f64)> {
        match self {
            Dash::Solid => None,
            Dash::Dash => Some(((DASH_LEN * bw - lw).max(0.0), DASH_GAP * bw + lw)),
            Dash::Dot => Some((0.0, DOT_GAP * bw + lw)),
        }
    }

    /// The menu's name for it.
    pub fn menu_key(self) -> &'static str {
        self.key().unwrap_or("solid")
    }
    pub fn from_menu_key(k: &str) -> Option<Dash> {
        [Dash::Solid, Dash::Dash, Dash::Dot].into_iter().find(|d| d.menu_key() == k)
    }

    pub fn key(self) -> Option<&'static str> {
        match self {
            Dash::Solid => None,
            Dash::Dash => Some("dash"),
            Dash::Dot => Some("dot"),
        }
    }
}

/// The style and grouping an item carries in its `sh`, apart from its shape: groups (outer
/// first), the line style, a box's fill colour, and bold/italic text. The default is none.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Style {
    pub g: Vec<String>,
    pub d: Dash,
    pub fc: Option<String>,
    pub b: bool,
    pub i: bool,
}

/// The deepest a group may nest, as the relay allows.
pub const MAX_GROUP_DEPTH: usize = 8;

impl Style {
    /// Read leniently from an item's `sh`: what is not understood is left at its default.
    pub fn from_value(v: Option<&Value>) -> Style {
        let Some(v) = v else { return Style::default() };
        let g: Vec<String> = v
            .get("g")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).take(MAX_GROUP_DEPTH).collect())
            .unwrap_or_default();
        let d = match v.get("d").and_then(Value::as_str) {
            Some("dash") => Dash::Dash,
            Some("dot") => Dash::Dot,
            _ => Dash::Solid,
        };
        let fc = v.get("fc").and_then(Value::as_str).filter(|c| is_colour(c)).map(str::to_ascii_lowercase);
        let flag = |k| v.get(k).and_then(Value::as_bool).unwrap_or(false);
        Style { g, d, fc, b: flag("b"), i: flag("i") }
    }

    /// The outermost group, which a click selects.
    pub fn top(&self) -> Option<&str> {
        self.g.first().map(String::as_str)
    }
}

/// `#rrggbb`.
pub fn is_colour(c: &str) -> bool {
    c.len() == 7 && c.starts_with('#') && c[1..].bytes().all(|b| b.is_ascii_hexdigit())
}

/// An item's `sh` as the relay writes it back (the canonical form), so a round trip is
/// unchanged: a shape or line with its style, text with b/i/g, or ink with groups. `None`
/// when there is nothing to say (plain text or ink).
pub fn sh_json(sh: Option<&Shape>, st: &Style, text: bool) -> Option<J> {
    let g = || ("g", J::Arr(st.g.iter().map(J::str).collect()));
    let mut f = match sh {
        Some(Shape::Box { kind, fill, n }) => {
            let mut f = vec![("k", J::str(kind.key())), ("n", J::str(n)), ("f", J::Bool(*fill))];
            if let Some(fc) = &st.fc {
                f.push(("fc", J::str(fc)));
            }
            f
        }
        Some(Shape::Line { h, a, b }) => {
            let mut f = vec![("k", J::str("line")), ("h", J::str(h.key()))];
            if let Some(a) = a {
                f.push(("a", a.to_json()));
            }
            if let Some(b) = b {
                f.push(("b", b.to_json()));
            }
            f
        }
        None if text => {
            if !st.b && !st.i && st.g.is_empty() {
                return None;
            }
            let mut f = vec![("k", J::str("text"))];
            if st.b {
                f.push(("b", J::Bool(true)));
            }
            if st.i {
                f.push(("i", J::Bool(true)));
            }
            if !st.g.is_empty() {
                f.push(g());
            }
            return Some(J::obj(f));
        }
        None => {
            if st.g.is_empty() {
                return None;
            }
            return Some(J::obj([("k", J::str("ink")), g()]));
        }
    };
    if let Some(d) = st.d.key() {
        f.push(("d", J::str(d)));
    }
    if !st.g.is_empty() {
        f.push(g());
    }
    Some(J::obj(f))
}

#[derive(Clone, Debug, PartialEq)]
pub enum Shape {
    Box { kind: BoxKind, fill: bool, n: String },
    Line { h: Heads, a: Option<Anchor>, b: Option<Anchor> },
}

impl Shape {
    /// The `sh` of an item. A shape this page cannot read is `None`, and the item is then
    /// drawn as the two-point stroke its `pts` make.
    pub fn from_value(v: &Value) -> Option<Shape> {
        match v.get("k")?.as_str()? {
            "line" => {
                let h = match v.get("h") {
                    None => Heads::End,
                    Some(h) => Heads::from_key(h.as_str()?)?,
                };
                let end = |k: &str| match v.get(k) {
                    None => Some(None),
                    Some(e) => Anchor::from_value(e).map(Some),
                };
                Some(Shape::Line { h, a: end("a")?, b: end("b")? })
            }
            k => Some(Shape::Box {
                kind: BoxKind::from_key(k)?,
                fill: v.get("f").and_then(Value::as_bool).unwrap_or(false),
                n: v.get("n")?.as_str()?.to_string(),
            }),
        }
    }

    /// As the relay writes it back (the canonical form), with no style.
    pub fn to_json(&self) -> J {
        sh_json(Some(self), &Style::default(), false).expect("a shape always has an sh")
    }

    pub fn node(&self) -> Option<&str> {
        match self {
            Shape::Box { n, .. } => Some(n),
            Shape::Line { .. } => None,
        }
    }

    pub fn is_line(&self) -> bool {
        matches!(self, Shape::Line { .. })
    }

    /// Whether this line has an end on node `n`.
    pub fn touches(&self, n: &str) -> bool {
        match self {
            Shape::Line { a, b, .. } => [a, b].into_iter().flatten().any(|e| e.n == n),
            Shape::Box { .. } => false,
        }
    }
}

/// The box of a shape's `pts`, as `[x0, y0, x1, y1]` with `x0 <= x1` and `y0 <= y1`.
pub fn norm_box(pts: &[f64]) -> Option<[f64; 4]> {
    if pts.len() < 6 {
        return None;
    }
    let (ax, ay, bx, by) = (pts[0], pts[1], pts[3], pts[4]);
    Some([ax.min(bx), ay.min(by), ax.max(bx), ay.max(by)])
}

/// The point at side `s`, `t` along it, on a shape of `kind` in box `bb`. Sides run
/// clockwise: a box's top, right, bottom, left; an ellipse's quarter arcs around the top,
/// right, bottom and left points; a triangle's right edge, base, left edge (apex at the top
/// centre). `t = 0.5` is the middle of the side.
pub fn anchor_point(kind: BoxKind, bb: &[f64; 4], s: u8, t: f64) -> (f64, f64) {
    let [x0, y0, x1, y1] = *bb;
    let (cx, cy) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
    let lerp = |a: (f64, f64), b: (f64, f64)| (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t);
    match kind {
        BoxKind::Rect => match s % 4 {
            0 => lerp((x0, y0), (x1, y0)),
            1 => lerp((x1, y0), (x1, y1)),
            2 => lerp((x1, y1), (x0, y1)),
            _ => lerp((x0, y1), (x0, y0)),
        },
        BoxKind::Ellipse => {
            // y grows downwards, so the top is at -pi/2 and clockwise is increasing angle
            let a = -FRAC_PI_2 + (s % 4) as f64 * FRAC_PI_2 + (t - 0.5) * FRAC_PI_2;
            (cx + (x1 - x0) / 2.0 * a.cos(), cy + (y1 - y0) / 2.0 * a.sin())
        }
        BoxKind::Tri => {
            let (ap, br, bl) = ((cx, y0), (x1, y1), (x0, y1));
            match s.min(2) {
                0 => lerp(ap, br),
                1 => lerp(br, bl),
                _ => lerp(bl, ap),
            }
        }
    }
}

/// The anchors a side offers: its middle, or every sub-anchor once it is expanded.
pub fn side_ts(expanded: bool) -> Vec<f64> {
    if expanded {
        (1..SUBS).map(|k| k as f64 / SUBS as f64).collect()
    } else {
        vec![0.5]
    }
}

/// One shape the scene knows: its kind and box.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Node {
    pub kind: BoxKind,
    pub bb: [f64; 4],
}

/// One line as drawn: its ends after attaching, its width and heads.
#[derive(Clone, Debug, PartialEq)]
pub struct Seg {
    pub id: String,
    pub p: [f64; 4],
    pub w: f64,
    pub h: Heads,
    ends: [Option<Anchor>; 2],
}

/// What drawing a line needs to know about the rest of the board: where every shape is, and
/// every line below it (for the hops where it crosses them). Built from the visible items, in
/// board order (the latest item with a node name wins).
#[derive(Clone, Debug, Default)]
pub struct Scene {
    pub nodes: HashMap<String, Node>,
    /// Node names that exist only on a hidden layer: a line on them hides too.
    hidden_nodes: HashSet<String>,
    /// Each node's place in the board order, for which is on top.
    z: HashMap<String, usize>,
    segs: Vec<Seg>,
    index: HashMap<String, usize>,
}

impl Scene {
    /// `hidden(item)` says whether the item is on a hidden layer.
    pub fn build<'a>(items: impl IntoIterator<Item = &'a Item> + Clone, hidden: impl Fn(&Item) -> bool) -> Scene {
        let mut sc = Scene::default();
        // A name belongs to the layer that used it first (S1): another layer's box with the
        // same name is drawn but holds no arrows, so it cannot take over someone's lines.
        let mut owner: HashMap<&str, Option<i64>> = HashMap::new();
        for (i, it) in items.clone().into_iter().enumerate() {
            if let (Some(Shape::Box { kind, n, .. }), Some(bb)) = (&it.sh, norm_box(&it.pts)) {
                if *owner.entry(n.as_str()).or_insert(it.l) != it.l {
                    continue;
                }
                if hidden(it) {
                    sc.hidden_nodes.insert(n.clone());
                } else {
                    sc.nodes.insert(n.clone(), Node { kind: *kind, bb });
                    sc.z.insert(n.clone(), i);
                }
            }
        }
        for it in items {
            if it.sh.as_ref().is_some_and(Shape::is_line) && !hidden(it) {
                if let Some(seg) = sc.resolve(it) {
                    sc.index.insert(it.id.clone(), sc.segs.len());
                    sc.segs.push(seg);
                }
            }
        }
        sc
    }

    /// A line's ends: attached ends at their anchor, free ends where they were drawn. `None`
    /// when it is not a line, has too few points, or an end is on a hidden shape.
    pub fn resolve(&self, it: &Item) -> Option<Seg> {
        self.resolve_with(it, None)
    }

    /// [`Scene::resolve`], as if node `moved.0` were at `moved.1` (a shape being dragged).
    pub fn resolve_with(&self, it: &Item, moved: Option<(&str, &Node)>) -> Option<Seg> {
        self.resolve_in(it, |n| moved.filter(|m| m.0 == n).map(|m| *m.1))
    }

    /// [`Scene::resolve`], with the nodes `moved` gives standing where it says (a selection
    /// being dragged).
    pub fn resolve_in(&self, it: &Item, moved: impl Fn(&str) -> Option<Node>) -> Option<Seg> {
        let Some(Shape::Line { h, a, b }) = &it.sh else { return None };
        if it.pts.len() < 6 {
            return None;
        }
        let end = |e: &Option<Anchor>, x: f64, y: f64| -> Option<(f64, f64)> {
            match e {
                None => Some((x, y)),
                Some(e) => match moved(&e.n).or_else(|| self.nodes.get(&e.n).copied()) {
                    Some(nd) => Some(anchor_point(nd.kind, &nd.bb, e.s, e.t)),
                    None if self.hidden_nodes.contains(&e.n) => None,
                    // the shape is gone (erased by its owner): the end stays where it was drawn
                    None => Some((x, y)),
                },
            }
        };
        let (ax, ay) = end(a, it.pts[0], it.pts[1])?;
        let (bx, by) = end(b, it.pts[3], it.pts[4])?;
        Some(Seg { id: it.id.clone(), p: [ax, ay, bx, by], w: it.w, h: *h, ends: [a.clone(), b.clone()] })
    }

    pub fn seg(&self, id: &str) -> Option<&Seg> {
        self.index.get(id).map(|&i| &self.segs[i])
    }

    /// The topmost shape within `near` (board units) of `(x, y)`, other than `except`.
    pub fn top_node(&self, x: f64, y: f64, near: f64, except: Option<&str>) -> Option<(&str, &Node)> {
        self.nodes
            .iter()
            .filter(|(n, _)| Some(n.as_str()) != except)
            .filter(|(_, nd)| {
                let [x0, y0, x1, y1] = nd.bb;
                x >= x0 - near && x <= x1 + near && y >= y0 - near && y <= y1 + near
            })
            .max_by_key(|(n, _)| self.z.get(*n).copied().unwrap_or(0))
            .map(|(n, nd)| (n.as_str(), nd))
    }

    /// Whether any visible line is attached to node `n` (moving it means a full redraw).
    pub fn attached(&self, n: &str) -> bool {
        self.segs.iter().any(|s| s.ends.iter().flatten().any(|e| e.n == n))
    }

    /// The hops on line `id`: where it crosses a line below it, as distances (board units)
    /// from its start, sorted. Only the upper line of a crossing hops, so each crossing is
    /// drawn once.
    pub fn hops(&self, id: &str, bw: f64) -> Vec<f64> {
        let Some(&i) = self.index.get(id) else { return vec![] };
        let me = &self.segs[i];
        let [ax, ay, bx, by] = me.p;
        let len = (bx - ax).hypot(by - ay);
        if len == 0.0 {
            return vec![];
        }
        let r = hop_r(me.w, bw) / bw;
        let (h0, h1) = (
            if me.h.at_start() { head_len(me.w, bw) / bw } else { 0.0 },
            if me.h.at_end() { head_len(me.w, bw) / bw } else { 0.0 },
        );
        let mut out: Vec<f64> = self.segs[..i]
            .iter()
            .filter(|o| !shares_end(me, o))
            .filter_map(|o| cross(&me.p, &o.p))
            .map(|u| u * len)
            .filter(|d| *d > h0 + r && *d < len - h1 - r)
            .collect();
        out.sort_by(f64::total_cmp);
        // hops that would overlap are drawn as the first of them
        let mut kept: Vec<f64> = Vec::with_capacity(out.len());
        for d in out {
            if kept.last().is_none_or(|k| d - k >= 2.0 * r) {
                kept.push(d);
            }
        }
        kept
    }

    /// A line's box, board units, with room for its heads and hops.
    pub fn bbox(&self, id: &str, bw: f64) -> Option<[f64; 4]> {
        let s = self.seg(id)?;
        let m = (head_len(s.w, bw).max(hop_r(s.w, bw)) + line_w(s.w, bw, 0.5)) / bw;
        let [ax, ay, bx, by] = s.p;
        Some([ax.min(bx) - m, ay.min(by) - m, ax.max(bx) + m, ay.max(by) + m])
    }
}

/// Two lines with an end on the same anchor meet there; that is not a crossing.
fn shares_end(a: &Seg, b: &Seg) -> bool {
    a.ends.iter().flatten().any(|x| b.ends.iter().flatten().any(|y| x.same(y)))
}

/// Where segment `a` properly crosses segment `b`, as a fraction along `a`.
fn cross(a: &[f64; 4], b: &[f64; 4]) -> Option<f64> {
    let (rx, ry) = (a[2] - a[0], a[3] - a[1]);
    let (sx, sy) = (b[2] - b[0], b[3] - b[1]);
    let d = rx * sy - ry * sx;
    if d.abs() < 1e-12 {
        return None; // parallel
    }
    let (qx, qy) = (b[0] - a[0], b[1] - a[1]);
    let u = (qx * sy - qy * sx) / d;
    let v = (qx * ry - qy * rx) / d;
    ((0.0..=1.0).contains(&u) && (0.0..=1.0).contains(&v)).then_some(u)
}

/// A hop's radius in board px: a little more than the line is thick.
pub fn hop_r(w: f64, bw: f64) -> f64 {
    2.0 * line_w(w, bw, 0.5) + 3.0
}

/// An arrowhead's length in board px.
pub fn head_len(w: f64, bw: f64) -> f64 {
    3.0 * line_w(w, bw, 0.5) + 8.0
}

/// A shape's path, then fill (and stroke the edge, so a filled shape is the same size as an
/// outlined one) or stroke.
/// A `#rrggbb` colour as 0xrrggbb.
pub fn rgb(c: &str) -> Option<u32> {
    let h = c.strip_prefix('#').filter(|h| h.len() == 6)?;
    u32::from_str_radix(h, 16).ok()
}

/// A shape: its outline, filled with its own colour when `fill`, or with `fc` (a bucket fill,
/// which wins) under the outline.
pub fn box_ops(kind: BoxKind, bb: &[f64; 4], fill: bool, fc: Option<&str>, d: Dash, w: f64, bw: f64) -> Vec<Op> {
    let [x0, y0, x1, y1] = bb.map(|v| v * bw);
    let lw = line_w(w, bw, 0.5);
    let mut o = vec![Op::LineWidth(lw), Op::Begin];
    match kind {
        BoxKind::Rect => o.extend([Op::MoveTo(x0, y0), Op::LineTo(x1, y0), Op::LineTo(x1, y1), Op::LineTo(x0, y1), Op::Close]),
        BoxKind::Ellipse => o.push(Op::Ellipse((x0 + x1) / 2.0, (y0 + y1) / 2.0, (x1 - x0) / 2.0, (y1 - y0) / 2.0)),
        BoxKind::Tri => o.extend([Op::MoveTo((x0 + x1) / 2.0, y0), Op::LineTo(x1, y1), Op::LineTo(x0, y1), Op::Close]),
    }
    match fc.and_then(rgb) {
        Some(c) => o.push(Op::FillColor(c)),
        None if fill => o.push(Op::Fill),
        None => {}
    }
    dashed(&mut o, d, lw, bw);
    o
}

/// The stroke, dashed as `d` asks and solid again after it.
fn dashed(o: &mut Vec<Op>, d: Dash, lw: f64, bw: f64) {
    match d.pattern(lw, bw) {
        Some((a, b)) => o.extend([Op::Dash(a, b), Op::Stroke, Op::Dash(0.0, 0.0)]),
        None => o.push(Op::Stroke),
    }
}

/// A line: its stroke with a hop at each of `hops` (distances from its start, board units),
/// then its heads.
pub fn line_ops(p: &[f64; 4], w: f64, h: Heads, hops: &[f64], d: Dash, bw: f64) -> Vec<Op> {
    let [ax, ay, bx, by] = p.map(|v| v * bw);
    let len = (bx - ax).hypot(by - ay);
    let lw = line_w(w, bw, 0.5);
    let mut o = vec![Op::LineWidth(lw), Op::Begin];
    if len == 0.0 {
        return vec![Op::Disc(ax, ay, lw / 2.0)];
    }
    let (dx, dy) = ((bx - ax) / len, (by - ay) / len);
    let hl = head_len(w, bw);
    // the stroke stops inside a head, so its round cap does not poke out of the tip
    let (s0, s1) = (if h.at_start() { hl * 0.8 } else { 0.0 }, if h.at_end() { len - hl * 0.8 } else { len });
    let at = |d: f64| (ax + dx * d, ay + dy * d);
    let (x, y) = at(s0);
    o.push(Op::MoveTo(x, y));
    let r = hop_r(w, bw);
    let th = dy.atan2(dx);
    for &d in hops {
        let d = d * bw;
        let (x, y) = at(d - r);
        o.push(Op::LineTo(x, y));
        let (cx, cy) = at(d);
        // clockwise from behind to ahead: over the top when the line runs left to right
        o.push(Op::Arc(cx, cy, r, th + PI, th + 2.0 * PI, false));
    }
    let (x, y) = at(s1);
    o.push(Op::LineTo(x, y));
    dashed(&mut o, d, lw, bw);
    let head = |o: &mut Vec<Op>, tx: f64, ty: f64, ux: f64, uy: f64| {
        let (bx, by) = (tx - ux * hl, ty - uy * hl);
        let hw = hl * 0.45;
        o.extend([
            Op::Begin,
            Op::MoveTo(tx, ty),
            Op::LineTo(bx - uy * hw, by + ux * hw),
            Op::LineTo(bx + uy * hw, by - ux * hw),
            Op::Close,
            Op::Fill,
        ]);
    };
    if h.at_end() {
        head(&mut o, bx, by, dx, dy);
    }
    if h.at_start() {
        head(&mut o, ax, ay, -dx, -dy);
    }
    o
}

/// How to draw item `it` if it is a shape or a line: `None` for anything else, or for a line
/// the scene does not show (an end on a hidden shape). `bb` is where it is, board units.
pub fn item_ops(it: &Item, scene: &Scene, bw: f64) -> Option<(Vec<Op>, Option<[f64; 4]>)> {
    match it.sh.as_ref()? {
        Shape::Box { kind, fill, .. } => Some((box_ops(*kind, &norm_box(&it.pts)?, *fill, it.st.fc.as_deref(), it.st.d, it.w, bw), None)),
        Shape::Line { .. } => {
            let s = scene.seg(&it.id)?;
            Some((line_ops(&s.p, s.w, s.h, &scene.hops(&it.id, bw), it.st.d, bw), scene.bbox(&it.id, bw)))
        }
    }
}

/// `t` as the wire keeps it: four decimals.
pub fn round_t(t: f64) -> f64 {
    (t * 1e4).round() / 1e4
}

/// Every anchor a shape shows: each side's middle, or its sub-anchors when `expanded` is that
/// side.
pub fn anchors(nd: &Node, expanded: Option<u8>) -> Vec<(u8, f64, (f64, f64))> {
    let mut out = vec![];
    for s in 0..nd.kind.sides() {
        for t in side_ts(expanded == Some(s)) {
            out.push((s, t, anchor_point(nd.kind, &nd.bb, s, t)));
        }
    }
    out
}

/// The anchor nearest `(x, y)` within snap distance (board units), or for a point inside
/// the shape, the nearest anchor whatever the distance (dropping a line on a shape attaches
/// it).
///
/// `grab_inside` false asks only for an anchor within snap distance (resting on one).
pub fn snap(nd: &Node, expanded: Option<u8>, x: f64, y: f64, snap: f64, sub_snap: f64, grab_inside: bool) -> Option<(u8, f64)> {
    let [x0, y0, x1, y1] = nd.bb;
    let inside = grab_inside && x >= x0 && x <= x1 && y >= y0 && y <= y1;
    let mut best: Option<(f64, u8, f64)> = None;
    for (s, t, (ax, ay)) in anchors(nd, expanded) {
        let d = (ax - x).hypot(ay - y);
        let lim = if expanded == Some(s) { sub_snap } else { snap };
        if (inside || d <= lim) && best.is_none_or(|b| d < b.0) {
            best = Some((d, s, t));
        }
    }
    best.map(|(_, s, t)| (s, t))
}

/// The progressive anchors' state while a line end hovers over shapes.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Hover {
    /// The side whose middle anchor the end is resting on, and since when.
    pub dwell: Option<(String, u8, f64)>,
    /// The side showing its sub-anchors.
    pub expanded: Option<(String, u8)>,
}

impl Hover {
    /// The expanded side of node `n`, if any.
    pub fn side_of(&self, n: &str) -> Option<u8> {
        self.expanded.as_ref().filter(|(e, _)| e == n).map(|(_, s)| *s)
    }

    /// The end moved to `(x, y)` at `now`; `on` is the anchor it is on now, if any. Rest on a
    /// side's middle anchor for [`DWELL_MS`], then move off it, and that side expands.
    pub fn step(&mut self, node: Option<&str>, on: Option<(u8, f64)>, now: f64) {
        let Some(n) = node else {
            *self = Hover::default();
            return;
        };
        if self.expanded.as_ref().is_some_and(|(e, _)| e != n) {
            self.expanded = None;
        }
        let mid = on.filter(|(s, t)| (t - 0.5).abs() < 1e-9 && self.side_of(n) != Some(*s)).map(|(s, _)| s);
        match (&self.dwell, mid) {
            (Some((dn, ds, _)), Some(s)) if dn == n && *ds == s => {} // still resting
            (Some((dn, ds, since)), _) => {
                if dn == n && now - since >= DWELL_MS {
                    self.expanded = Some((n.to_string(), *ds));
                }
                self.dwell = mid.map(|s| (n.to_string(), s, now));
            }
            (None, Some(s)) => self.dwell = Some((n.to_string(), s, now)),
            (None, None) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn item(id: &str, pts: Vec<f64>, sh: Value) -> Item {
        Item { id: id.into(), l: Some(1), q: None, c: "#111111".into(), w: 0.004, pts, txt: None, sh: Shape::from_value(&sh), st: Default::default() }
    }

    #[test]
    fn shapes_round_trip_in_the_relays_canonical_form() {
        for v in [
            json!({"k": "rect", "n": "nA", "f": true}),
            json!({"k": "ellipse", "n": "nB", "f": false}),
            json!({"k": "tri", "n": "nC", "f": false}),
            json!({"k": "line", "h": "none"}),
            json!({"k": "line", "h": "both", "a": {"n": "nA", "s": 1, "t": 0.5}, "b": {"n": "nB", "s": 3, "t": 0.1875}}),
            json!({"k": "line", "h": "start", "b": {"n": "nB", "s": 0, "t": 0.0625}}),
        ] {
            let sh = Shape::from_value(&v).expect("reads");
            assert_eq!(serde_json::from_str::<Value>(&sh.to_json().stringify()).unwrap(), v);
        }
        // defaults: an outline, a head at the end
        assert_eq!(Shape::from_value(&json!({"k": "rect", "n": "a"})), Some(Shape::Box { kind: BoxKind::Rect, fill: false, n: "a".into() }));
        assert_eq!(Shape::from_value(&json!({"k": "line"})), Some(Shape::Line { h: Heads::End, a: None, b: None }));
        assert_eq!(Shape::from_value(&json!({"k": "star", "n": "a"})), None);
        assert_eq!(Shape::from_value(&json!({"k": "line", "a": {"n": "a", "s": 9, "t": 0.5}})), None);
    }

    #[test]
    fn styles_and_groups_round_trip_in_the_relays_canonical_form() {
        let st = |v: &Value| Style::from_value(Some(v));
        for (v, text) in [
            (json!({"k": "rect", "n": "nA", "f": true, "fc": "#aabbcc", "d": "dot", "g": ["G1", "G2"]}), false),
            (json!({"k": "line", "h": "end", "a": {"n": "nA", "s": 0, "t": 0}, "d": "dash"}), false),
            (json!({"k": "text", "b": true, "g": ["G1"]}), true),
            (json!({"k": "text", "i": true}), true),
            (json!({"k": "ink", "g": ["G1"]}), false),
        ] {
            let out = sh_json(Shape::from_value(&v).as_ref(), &st(&v), text).expect("has an sh");
            assert_eq!(serde_json::from_str::<Value>(&out.stringify()).unwrap(), v);
        }
        // nothing to say: plain text and ink carry no sh
        assert!(sh_json(None, &Style::default(), true).is_none());
        assert!(sh_json(None, &Style::default(), false).is_none());
        // read leniently: a bad colour or dash is left at the default
        assert_eq!(st(&json!({"k": "rect", "n": "a", "fc": "red", "d": "wavy"})), Style::default());
        assert_eq!(st(&json!({"fc": "#ABCDEF"})).fc.as_deref(), Some("#abcdef"));
    }

    #[test]
    fn a_node_name_belongs_to_the_layer_that_used_it_first() {
        let on = |l, it: Item| Item { l: Some(l), ..it };
        let a = on(1, item("a1", vec![0.0, 0.0, 0.5, 1.0, 1.0, 0.5], json!({"k": "rect", "n": "A"})));
        // another contributor's box with the same name, drawn later and elsewhere
        let b = on(2, item("b1", vec![5.0, 5.0, 0.5, 6.0, 6.0, 0.5], json!({"k": "rect", "n": "A"})));
        let l = on(2, item("l1", vec![0.0, 0.0, 0.5, 9.0, 9.0, 0.5], json!({"k": "line", "a": {"n": "A", "s": 1, "t": 0.5}})));
        let sc = Scene::build([&a, &b, &l], |_| false);
        assert_eq!(sc.seg("l1").unwrap().p[..2], [1.0, 0.5], "the line stays on the first layer's box");
        // and while that layer is hidden, its name stays its own: the line hides with it
        let sc = Scene::build([&a, &b, &l], |it| it.l == Some(1));
        assert!(sc.seg("l1").is_none());
        // the owner moving its box (a later item, same layer, same name) is followed
        let a2 = on(1, item("a2", vec![2.0, 0.0, 0.5, 3.0, 1.0, 0.5], json!({"k": "rect", "n": "A"})));
        let sc = Scene::build([&a, &b, &a2, &l], |_| false);
        assert_eq!(sc.seg("l1").unwrap().p[..2], [3.0, 0.5]);
    }

    #[test]
    fn anchors_sit_on_the_outline_and_stay_relative_to_the_box() {
        let bb = [0.0, 0.0, 2.0, 1.0];
        assert_eq!(anchor_point(BoxKind::Rect, &bb, 0, 0.5), (1.0, 0.0));
        assert_eq!(anchor_point(BoxKind::Rect, &bb, 1, 0.5), (2.0, 0.5));
        assert_eq!(anchor_point(BoxKind::Rect, &bb, 2, 0.25), (1.5, 1.0));
        assert_eq!(anchor_point(BoxKind::Tri, &bb, 1, 0.5), (1.0, 1.0));
        let (x, y) = anchor_point(BoxKind::Ellipse, &bb, 1, 0.5);
        assert!((x - 2.0).abs() < 1e-12 && (y - 0.5).abs() < 1e-12);
        // moved and stretched, the same anchor is at the same relative place
        let moved = [1.0, 1.0, 5.0, 3.0];
        assert_eq!(anchor_point(BoxKind::Rect, &moved, 2, 0.25), (4.0, 3.0));
        assert_eq!(anchors(&Node { kind: BoxKind::Tri, bb }, None).len(), 3);
        assert_eq!(anchors(&Node { kind: BoxKind::Rect, bb }, Some(2)).len(), 3 + 15);
    }

    #[test]
    fn a_line_follows_its_shapes_and_a_free_end_stays_put() {
        let a = item("a1", vec![0.0, 0.0, 0.5, 1.0, 1.0, 0.5], json!({"k": "rect", "n": "A"}));
        let b = item("b1", vec![3.0, 0.0, 0.5, 4.0, 1.0, 0.5], json!({"k": "ellipse", "n": "B"}));
        let l = item(
            "l1",
            vec![1.0, 0.5, 0.5, 3.0, 0.5, 0.5],
            json!({"k": "line", "a": {"n": "A", "s": 1, "t": 0.5}, "b": {"n": "B", "s": 3, "t": 0.5}}),
        );
        let free = item("l2", vec![9.0, 9.0, 0.5, 8.0, 8.0, 0.5], json!({"k": "line", "a": {"n": "A", "s": 2, "t": 0.5}}));
        let near = |p: [f64; 4], q: [f64; 4]| p.iter().zip(q).all(|(a, b)| (a - b).abs() < 1e-9);
        let sc = Scene::build([&a, &b, &l, &free], |_| false);
        assert!(near(sc.seg("l1").unwrap().p, [1.0, 0.5, 3.0, 0.5]));
        // B moves (an undo and a new item with the same node): the line's end goes with it
        let b2 = item("b2", vec![3.0, 2.0, 0.5, 4.0, 4.0, 0.5], json!({"k": "ellipse", "n": "B"}));
        let sc = Scene::build([&a, &l, &free, &b2], |_| false);
        let p = sc.seg("l1").unwrap().p;
        assert!((p[2] - 3.0).abs() < 1e-12 && (p[3] - 3.0).abs() < 1e-12, "{p:?}");
        assert_eq!(sc.seg("l2").unwrap().p, [0.5, 1.0, 8.0, 8.0]);
        assert!(sc.attached("B") && sc.attached("A") && !sc.attached("C"));
        // a shape on a hidden layer hides its lines; an erased one leaves the drawn end
        let sc = Scene::build([&a, &b, &l], |it| it.id == "b1");
        assert!(sc.seg("l1").is_none());
        let sc = Scene::build([&a, &l], |_| false);
        assert_eq!(sc.seg("l1").unwrap().p, [1.0, 0.5, 3.0, 0.5]);
    }

    #[test]
    fn crossing_lines_hop_once_on_the_upper_one() {
        let h = item("h", vec![0.0, 0.5, 0.5, 1.0, 0.5, 0.5], json!({"k": "line", "h": "none"}));
        let v = item("v", vec![0.5, 0.0, 0.5, 0.5, 1.0, 0.5], json!({"k": "line", "h": "none"}));
        let sc = Scene::build([&h, &v], |_| false);
        let bw = 1000.0;
        assert!(sc.hops("h", bw).is_empty());
        assert_eq!(sc.hops("v", bw), vec![0.5]);
        let arcs = |id: &str| {
            let s = sc.seg(id).unwrap();
            line_ops(&s.p, s.w, s.h, &sc.hops(id, bw), Dash::Solid, bw).iter().filter(|o| matches!(o, Op::Arc(..))).count()
        };
        assert_eq!(arcs("h") + arcs("v"), 1);
        // lines that meet at a shared anchor do not hop
        let r = item("r", vec![0.4, 0.4, 0.5, 0.6, 0.6, 0.5], json!({"k": "rect", "n": "R"}));
        let x = item("x", vec![0.0, 0.0, 0.5, 0.5, 0.4, 0.5], json!({"k": "line", "b": {"n": "R", "s": 0, "t": 0.5}}));
        let y = item("y", vec![1.0, 0.0, 0.5, 0.5, 0.4, 0.5], json!({"k": "line", "b": {"n": "R", "s": 0, "t": 0.5}}));
        let sc = Scene::build([&r, &x, &y], |_| false);
        assert!(sc.hops("y", bw).is_empty());
    }

    #[test]
    fn a_rested_anchor_expands_its_side_when_left() {
        let mut h = Hover::default();
        h.step(Some("A"), Some((1, 0.5)), 0.0);
        h.step(Some("A"), Some((1, 0.5)), 500.0);
        h.step(Some("A"), None, 900.0); // left too soon
        assert_eq!(h.expanded, None);
        h.step(Some("A"), Some((1, 0.5)), 1000.0);
        h.step(Some("A"), Some((1, 0.5)), 2700.0);
        assert_eq!(h.expanded, None, "not while still on it");
        h.step(Some("A"), None, 2800.0);
        assert_eq!(h.expanded, Some(("A".into(), 1)));
        assert_eq!(h.side_of("A"), Some(1));
        // the sub-anchors snap; another shape resets it
        let nd = Node { kind: BoxKind::Rect, bb: [0.0, 0.0, 1.0, 1.0] };
        assert_eq!(snap(&nd, Some(1), 1.0, 0.25, 0.1, 0.05, false), Some((1, 0.25)));
        h.step(None, None, 2900.0);
        assert_eq!(h, Hover::default());
    }
    #[test]
    fn dashes_and_dots_keep_the_same_gap_at_any_thickness() {
        let bw = 1000.0;
        let pat = |ops: &[Op]| {
            let lw = ops.iter().find_map(|o| if let Op::LineWidth(w) = o { Some(*w) } else { None }).unwrap();
            let d = ops.iter().find_map(|o| if let Op::Dash(a, g) = o { Some((*a, *g)) } else { None });
            (lw, d)
        };
        let p = [0.1, 0.1, 0.5, 0.1];
        let mut gaps = vec![];
        for w in [0.002, 0.009] { // the small and large pens
            let (lw, d) = pat(&line_ops(&p, w, Heads::None, &[], Dash::Dash, bw));
            let (a, g) = d.unwrap();
            // a round cap adds lw/2 at each end: what shows is dash a + lw, gap g - lw
            assert!((a + lw - DASH_LEN * bw).abs() < 1e-9, "dash {a} at lw {lw}");
            gaps.push(g - lw);
            let (lw, d) = pat(&line_ops(&p, w, Heads::None, &[], Dash::Dot, bw));
            let (a, g) = d.unwrap();
            assert_eq!(a, 0.0, "a dot is a round cap");
            assert!((g - lw - DOT_GAP * bw).abs() < 1e-9);
            // a dashed box: dashed outline, solid again after it
            let ops = box_ops(BoxKind::Rect, &[0.1, 0.1, 0.3, 0.3], false, None, Dash::Dash, w, bw);
            let n = ops.len();
            let (a, g) = Dash::Dash.pattern(lw, bw).unwrap();
            assert_eq!(&ops[n - 3..], &[Op::Dash(a, g), Op::Stroke, Op::Dash(0.0, 0.0)][..]);
        }
        assert!((gaps[0] - gaps[1]).abs() < 1e-9 && (gaps[0] - DASH_GAP * bw).abs() < 1e-9, "{gaps:?}");
        assert!(pat(&line_ops(&p, 0.004, Heads::End, &[], Dash::Solid, bw)).1.is_none(), "solid sets no dash");
        // the heads are drawn after the dash is cleared
        let ops = line_ops(&p, 0.004, Heads::End, &[], Dash::Dot, bw);
        let clear = ops.iter().position(|o| *o == Op::Dash(0.0, 0.0)).unwrap();
        assert!(ops.iter().rposition(|o| *o == Op::Fill).unwrap() > clear);
    }

}
