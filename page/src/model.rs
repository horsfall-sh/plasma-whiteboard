//! The board as this browser holds it: finished items in `q` order, the strokes still being
//! drawn, the layers, and this browser's items the relay has not confirmed yet.
//!
//! Nothing here draws. Each change says what the canvas needs (see [`Plan`] and the `bool`s
//! returned), so the rules the JavaScript page followed can be tested without a browser.

use crate::proto::{Item, Layer, LayerInfo, Out};
use indexmap::IndexMap;
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet, VecDeque};

/// `qOf`: an item the relay has not stored sorts after every stored one.
pub fn q_of(s: &Item) -> f64 {
    s.q.unwrap_or(f64::INFINITY)
}

/// `byQ`.
pub fn by_q(a: &Item, b: &Item) -> Ordering {
    q_of(a).partial_cmp(&q_of(b)).unwrap_or(Ordering::Equal)
}

/// A stroke still being drawn, mine or someone else's. `drawn` is how many of its points are
/// on the live canvas.
#[derive(Clone, Debug, PartialEq)]
pub struct Live {
    pub id: String,
    pub l: Option<Layer>,
    pub c: String,
    pub w: f64,
    pub pts: Vec<f64>,
    pub drawn: usize,
}

/// What to do on the canvas after [`Board::insert`] put an item at index `i`. This is the
/// branching of `insertStroke` over the redraw loop's `cursor` and whether it is `buffered`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Plan {
    /// Draw the item now (if its layer is shown).
    pub draw: bool,
    /// Then move the redraw cursor past it.
    pub advance: bool,
    /// Wake the sliced redraw loop.
    pub kick: bool,
    /// Redraw the whole base canvas.
    pub redraw: bool,
}

/// Where an inserted item went.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Placed {
    pub i: usize,
    pub len: usize,
    /// The item after it is unconfirmed: it went under my own pending strokes.
    pub tail_unconfirmed: bool,
}

impl Placed {
    pub fn plan(self, cursor: usize, buffered: bool, defer: bool) -> Plan {
        let Placed { i, len, tail_unconfirmed } = self;
        if i >= cursor {
            // not reached by the redraw loop yet
            if !buffered && !defer && i == cursor && i == len - 1 {
                Plan { draw: true, advance: true, ..Plan::default() }
            } else {
                // a buffered loop shows it now and adds it to the buffer when it gets there
                Plan { draw: buffered, kick: true, ..Plan::default() }
            }
        } else if tail_unconfirmed && !buffered {
            // under my own unconfirmed strokes: paint it on top for now; the order stays right
            Plan { draw: true, advance: true, ..Plan::default() }
        } else {
            Plan { redraw: true, ..Plan::default() }
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Board {
    pub strokes: Vec<Item>,
    ids: HashSet<String>,
    /// Items per layer (`counts`), keyed as the item's `l` is, which can be absent.
    pub counts: HashMap<Option<Layer>, i64>,
    pub live: IndexMap<String, Live>,
    pub layers: IndexMap<Layer, LayerInfo>,
    /// My items the relay has not confirmed, in the order I made them.
    pub pending: IndexMap<String, Item>,
    /// Undos asked for while the socket was down, sent after the next `ready`.
    pub undo_queue: Vec<String>,
}

impl Board {
    pub fn has(&self, id: &str) -> bool {
        self.ids.contains(id)
    }

    pub fn get(&self, id: &str) -> Option<&Item> {
        if !self.has(id) {
            return None;
        }
        self.strokes.iter().rev().find(|s| s.id == id)
    }

    fn bump(&mut self, l: Option<Layer>, d: i64) {
        *self.counts.entry(l).or_insert(0) += d;
    }

    pub fn count(&self, l: Layer) -> i64 {
        self.counts.get(&Some(l)).copied().unwrap_or(0)
    }

    /// `insertStroke`: in `q` order, after the items with the same `q`. `None` when the id is
    /// already on the board.
    pub fn insert(&mut self, s: Item) -> Option<Placed> {
        if self.has(&s.id) {
            return None;
        }
        let q = q_of(&s);
        let mut i = self.strokes.len();
        while i > 0 && q_of(&self.strokes[i - 1]) > q {
            i -= 1;
        }
        let tail_unconfirmed = i < self.strokes.len() && self.strokes[i].q.is_none();
        self.ids.insert(s.id.clone());
        self.bump(s.l, 1);
        self.strokes.insert(i, s);
        Some(Placed { i, len: self.strokes.len(), tail_unconfirmed })
    }

    /// `removeStroke`. Whether it was on the board (and so the base needs a redraw).
    pub fn remove(&mut self, id: &str) -> bool {
        if !self.ids.remove(id) {
            return false;
        }
        if let Some(i) = self.strokes.iter().rposition(|s| s.id == id) {
            let s = self.strokes.remove(i);
            self.bump(s.l, -1);
        }
        true
    }

    /// `dropBoard`.
    pub fn drop_all(&mut self) {
        self.strokes.clear();
        self.ids.clear();
        self.counts.clear();
        self.live.clear();
        self.layers.clear();
    }

    /// `begin`.
    pub fn begin(&mut self, id: &str, l: Option<Layer>, c: &str, w: f64) {
        self.live.insert(id.to_string(), Live { id: id.into(), l, c: c.into(), w, pts: vec![], drawn: 0 });
    }

    /// `extend`. The stroke when there is one to draw more of.
    pub fn extend(&mut self, id: &str, pts: &[f64]) -> Option<&mut Live> {
        let s = self.live.get_mut(id)?;
        s.pts.extend_from_slice(pts);
        Some(s)
    }

    /// Takes a live stroke off the live canvas. Whether there was one (the live canvas then
    /// needs a redraw). Keeps the order of the others, as `Map.delete` does.
    pub fn end_live(&mut self, id: &str) -> bool {
        self.live.shift_remove(id).is_some()
    }

    /// `ok`: the relay stored my item as `q`. `true` when `uploading` should end: the last
    /// item of a board that was private is on the relay now.
    pub fn confirm(&mut self, id: &str, q: Option<f64>, uploading: bool) -> bool {
        if self.pending.shift_remove(id).is_none() {
            return false;
        }
        if let Some(i) = self.strokes.iter().rposition(|s| s.id == id) {
            self.strokes[i].q = q;
            let qq = q.unwrap_or(f64::INFINITY);
            let n = self.strokes.len();
            if (i > 0 && q_of(&self.strokes[i - 1]) > qq) || (i + 1 < n && q_of(&self.strokes[i + 1]) < qq) {
                self.strokes.sort_by(by_q); // stable, as Array.prototype.sort is
            }
        }
        uploading && self.pending.is_empty()
    }

    /// `no`: the relay refused my item. Whether it was on the board.
    pub fn refused(&mut self, id: &str) -> bool {
        self.pending.shift_remove(id);
        self.remove(id)
    }

    /// `u` from the relay, unless it is the old socket's copy of the stroke I am drawing.
    /// Returns (live canvas needs a redraw, base needs a redraw).
    pub fn undone(&mut self, id: &str, cur: Option<&str>) -> (bool, bool) {
        if cur == Some(id) {
            return (false, false);
        }
        self.pending.shift_remove(id);
        (self.end_live(id), self.remove(id))
    }

    /// `l`: a new layer. Whether it is new to me.
    pub fn new_layer(&mut self, l: Layer, c: &str) -> bool {
        if self.layers.contains_key(&l) {
            return false;
        }
        self.layers.insert(l, LayerInfo { l, c: c.into(), del: false, n: None });
        true
    }

    /// `onLayerDeleted`: drops the layer's items except my unconfirmed ones. The ids that went
    /// (when any did, the base needs a redraw).
    pub fn layer_deleted(&mut self, l: Layer) -> Vec<String> {
        let mut gone = Vec::new();
        let (pending, ids) = (&self.pending, &mut self.ids);
        self.strokes.retain(|s| {
            let keep = s.l != Some(l) || pending.contains_key(&s.id);
            if !keep {
                ids.remove(&s.id);
                gone.push(s.id.clone());
            }
            keep
        });
        if !gone.is_empty() {
            self.bump(Some(l), -(gone.len() as i64));
        }
        self.layers.entry(l).or_insert_with(|| LayerInfo { l, c: "#888888".into(), del: true, n: None }).del = true;
        gone
    }

    /// `clearSheet`: the sheet goes, except my unconfirmed items. The ids that went.
    pub fn clear_sheet(&mut self) -> Vec<String> {
        let (keep, gone): (Vec<Item>, Vec<Item>) =
            std::mem::take(&mut self.strokes).into_iter().partition(|s| self.pending.contains_key(&s.id));
        self.ids.clear();
        self.counts.clear();
        for s in keep {
            self.ids.insert(s.id.clone());
            self.bump(s.l, 1);
            self.strokes.push(s);
        }
        gone.into_iter().map(|s| s.id).collect()
    }

    /// The board part of `onUncrumpled`, after [`Board::clear_sheet`]: the sheet's items in `q`
    /// order, then what was left of mine on top, as they were. An item already here is skipped
    /// (JavaScript kept `byId` while it took `mineLeft` out). `layers`, when the frame had them,
    /// are set over the ones known.
    pub fn uncrumpled(&mut self, a: Vec<Item>, layers: Option<Vec<LayerInfo>>) {
        let mine_left = std::mem::take(&mut self.strokes);
        for s in a {
            if self.has(&s.id) {
                continue;
            }
            self.ids.insert(s.id.clone());
            self.bump(s.l, 1);
            self.strokes.push(s);
        }
        self.strokes.sort_by(by_q);
        self.strokes.extend(mine_left);
        for info in layers.unwrap_or_default() {
            self.layers.insert(info.l, info);
        }
    }

    /// `onLayerRestored`. `a` is `None` when the frame carried no list. Whether the base needs a
    /// redraw.
    pub fn layer_restored(&mut self, l: Layer, a: Option<Vec<Item>>) -> bool {
        let redraw = if let Some(a) = a {
            for mut s in a {
                if self.has(&s.id) {
                    continue;
                }
                s.l.get_or_insert(l);
                self.ids.insert(s.id.clone());
                self.bump(s.l, 1);
                self.strokes.push(s);
            }
            self.strokes.sort_by(by_q);
            true
        } else {
            false
        };
        if let Some(info) = self.layers.get_mut(&l) {
            info.del = false;
        }
        redraw
    }

    /// The board part of `onHello`: everything is dropped (the snapshot follows) except the
    /// stroke I am drawing, which moves to my layer and is sent whole when it ends. `cur` is that
    /// stroke's id when I can still edit. Whether it was kept.
    pub fn hello(&mut self, cur: Option<&str>, me: Option<Layer>, layers: &[LayerInfo]) -> bool {
        let keep = cur.and_then(|id| self.live.get(id)).cloned();
        self.drop_all();
        let kept = keep.is_some();
        if let Some(mut s) = keep {
            s.l = me;
            self.live.insert(s.id.clone(), s);
        }
        for info in layers {
            self.layers.insert(info.l, info.clone());
        }
        kept
    }

    /// The board part of `onReady`. A viewer forgets what it had pending. An editor puts back
    /// the items the relay never confirmed (or takes the relay's copy when only the `ok` was
    /// lost), drops the queued undos, and gets the ids to send whole, in order. `me` is my layer
    /// only when I can edit (`role === 'edit' && me != null`).
    pub fn ready(&mut self, me: Option<Layer>) -> Ready {
        let Some(me) = me else {
            self.pending.clear();
            self.undo_queue.clear();
            return Ready::default();
        };
        let mut todo = VecDeque::new();
        let ids: Vec<String> = self.pending.keys().cloned().collect();
        for id in ids {
            if let Some(have) = self.get(&id).cloned() {
                self.pending.insert(id.clone(), have); // keeps its place, as Map.set does
            } else {
                let s = self.pending.get_mut(&id).expect("listed");
                s.l = Some(me);
                s.q = None;
                let s = s.clone();
                self.insert(s);
            }
            todo.push_back(id);
        }
        let undo = std::mem::take(&mut self.undo_queue);
        for id in &undo {
            self.remove(id);
        }
        Ready { todo, undo }
    }

    /// One `bite`: the next frames of `todo` the relay's rate limits allow (100 items or about
    /// 120 KB, counted as JavaScript counts `text.length`), skipping items no longer pending.
    pub fn bite(&self, todo: &mut VecDeque<String>) -> Vec<String> {
        let (mut out, mut bytes) = (Vec::new(), 0);
        while out.len() < 100 && bytes < 120_000 {
            let Some(id) = todo.pop_front() else { break };
            let Some(s) = self.pending.get(&id) else { continue };
            let text = Out::Stroke { id: s.id.clone(), c: s.c.clone(), w: s.w, pts: s.pts.clone(), txt: s.txt.clone(), sh: s.sh.clone(), st: s.st.clone() }.frame();
            bytes += text.encode_utf16().count();
            out.push(text);
        }
        out
    }
}

/// What `ready` leaves to send: the items, in bites, and the undos (`{t:'u',id}` each).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Ready {
    pub todo: VecDeque<String>,
    pub undo: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn it(id: &str, l: Layer, q: Option<f64>) -> Item {
        Item { id: id.into(), l: Some(l), q, c: "#111111".into(), w: 0.004, pts: vec![0.0, 0.0, 0.5], txt: None, sh: None, st: Default::default() }
    }
    fn order(b: &Board) -> Vec<&str> {
        b.strokes.iter().map(|s| s.id.as_str()).collect()
    }

    #[test]
    fn items_go_in_q_order_after_equals_and_unconfirmed_go_last() {
        let mut b = Board::default();
        b.insert(it("c", 1, Some(3.0)));
        b.insert(it("mine", 1, None));
        b.insert(it("a", 1, Some(1.0)));
        let p = b.insert(it("b", 2, Some(3.0))).unwrap();
        assert_eq!(order(&b), ["a", "c", "b", "mine"]);
        assert_eq!(p, Placed { i: 2, len: 4, tail_unconfirmed: true });
        assert_eq!(b.insert(it("a", 1, Some(9.0))), None, "an id is on the board once");
        assert_eq!((b.count(1), b.count(2)), (3, 1));
        assert!(b.remove("c") && !b.remove("c"));
        assert_eq!((order(&b), b.count(1)), (vec!["a", "b", "mine"], 2));
    }

    #[test]
    fn the_paint_plan_follows_insert_stroke() {
        let at = |i, len, tail| Placed { i, len, tail_unconfirmed: tail };
        let draw_advance = Plan { draw: true, advance: true, ..Plan::default() };
        // the next item at the end of a caught-up loop: draw it and move on
        assert_eq!(at(5, 6, false).plan(5, false, false), draw_advance);
        // a snapshot item, or one behind the loop's work, waits for the loop
        assert_eq!(at(5, 6, false).plan(5, false, true), Plan { kick: true, ..Plan::default() });
        assert_eq!(at(7, 9, false).plan(5, false, false), Plan { kick: true, ..Plan::default() });
        // a buffered loop shows it now as well
        assert_eq!(at(7, 9, false).plan(5, true, false), Plan { draw: true, kick: true, ..Plan::default() });
        // under my unconfirmed strokes, already drawn past: paint on top
        assert_eq!(at(2, 6, true).plan(5, false, false), draw_advance);
        // anything else already drawn past: redraw
        assert_eq!(at(2, 6, true).plan(5, true, false), Plan { redraw: true, ..Plan::default() });
        assert_eq!(at(2, 6, false).plan(5, false, false), Plan { redraw: true, ..Plan::default() });
    }

    #[test]
    fn an_ok_sets_q_and_resorts_only_when_out_of_order() {
        let mut b = Board::default();
        b.insert(it("a", 1, Some(1.0)));
        b.insert(it("m1", 1, None));
        b.insert(it("m2", 1, None));
        b.pending.insert("m1".into(), b.get("m1").unwrap().clone());
        b.pending.insert("m2".into(), b.get("m2").unwrap().clone());
        assert!(!b.confirm("m2", Some(4.0), true), "m1 still pending");
        assert_eq!(order(&b), ["a", "m2", "m1"]);
        assert!(b.confirm("m1", Some(5.0), true), "the last one ends the upload");
        assert_eq!(order(&b), ["a", "m2", "m1"]);
        assert_eq!(b.get("m1").unwrap().q, Some(5.0));
        assert!(!b.confirm("zz", Some(1.0), true), "not mine");
    }

    #[test]
    fn an_undo_spares_the_stroke_i_am_drawing() {
        let mut b = Board::default();
        b.begin("cur", Some(1), "#111111", 0.004);
        assert_eq!(b.undone("cur", Some("cur")), (false, false));
        assert_eq!(b.undone("cur", None), (true, false));
        b.insert(it("x", 1, Some(1.0)));
        assert_eq!(b.undone("x", None), (false, true));
    }

    #[test]
    fn a_deleted_layer_keeps_my_unconfirmed_items_and_a_restore_puts_the_rest_back_in_order() {
        let mut b = Board::default();
        b.new_layer(2, "#d63031");
        b.insert(it("a", 2, Some(1.0)));
        b.insert(it("b", 1, Some(2.0)));
        b.insert(it("c", 2, Some(3.0)));
        b.insert(it("m", 2, None));
        b.pending.insert("m".into(), b.get("m").unwrap().clone());
        assert_eq!(b.layer_deleted(2), ["a", "c"]);
        assert_eq!((order(&b), b.count(2)), (vec!["b", "m"], 1));
        assert!(b.layers[&2].del);
        assert!(b.layer_deleted(9).is_empty() && b.layers[&9].c == "#888888");
        let mut back = vec![it("c", 2, Some(3.0)), it("a", 2, Some(1.0)), it("m", 2, None)];
        back[1].l = None; // a restored item without a layer is on the restored layer
        assert!(b.layer_restored(2, Some(back)));
        assert_eq!((order(&b), b.count(2)), (vec!["a", "b", "c", "m"], 3));
        assert!(!b.layers[&2].del);
        assert!(!b.layer_restored(2, None));
    }

    #[test]
    fn hello_drops_the_board_but_keeps_the_stroke_i_am_drawing_on_my_new_layer() {
        let mut b = Board::default();
        b.insert(it("a", 1, Some(1.0)));
        b.begin("cur", Some(1), "#111111", 0.004);
        b.begin("theirs", Some(3), "#111111", 0.004);
        let layers = [LayerInfo { l: 4, c: "#0a66d8".into(), del: false, n: None }];
        assert!(b.hello(Some("cur"), Some(4), &layers));
        assert!(b.strokes.is_empty() && b.count(1) == 0);
        assert_eq!(b.live.keys().collect::<Vec<_>>(), ["cur"]);
        assert_eq!(b.live["cur"].l, Some(4));
        assert_eq!(b.layers.keys().collect::<Vec<_>>(), [&4]);
        assert!(!b.hello(None, None, &[]) && b.live.is_empty());
    }

    #[test]
    fn ready_resends_what_was_never_confirmed_in_order_and_the_bites_respect_the_limits() {
        let mut b = Board::default();
        // three pending from the old socket; the relay's snapshot already has "two"
        for id in ["one", "two", "three"] {
            b.pending.insert(id.into(), it(id, 1, None));
        }
        b.insert(it("two", 5, Some(8.0)));
        b.insert(it("gone", 5, Some(9.0)));
        b.undo_queue.push("gone".into());
        let mut r = b.ready(Some(5));
        assert_eq!(r.todo, ["one", "two", "three"]);
        assert_eq!(r.undo, ["gone"]);
        assert_eq!(order(&b), ["two", "one", "three"]);
        assert_eq!(b.get("one").unwrap().l, Some(5));
        assert_eq!(b.pending["two"].q, Some(8.0), "the relay's copy");
        b.pending.shift_remove("three");
        let frames = b.bite(&mut r.todo);
        assert_eq!(
            frames,
            [r##"{"t":"s","id":"one","c":"#111111","w":0.004,"pts":[0,0,0.5]}"##, r##"{"t":"s","id":"two","c":"#111111","w":0.004,"pts":[0,0,0.5]}"##]
        );
        assert!(r.todo.is_empty());

        // 250 small items go 100 a bite
        let mut b = Board::default();
        let mut todo: VecDeque<String> = (0..250).map(|i| format!("s{i}")).collect();
        for id in &todo {
            b.pending.insert(id.clone(), it(id, 1, None));
        }
        let sizes: Vec<usize> = std::iter::from_fn(|| Some(b.bite(&mut todo).len()).filter(|n| *n > 0)).collect();
        assert_eq!(sizes, [100, 100, 50]);

        // big items stop a bite once 120000 UTF-16 units are out (the item that crosses goes too)
        let mut b = Board::default();
        let mut todo: VecDeque<String> = (0..5).map(|i| format!("t{i}")).collect();
        for id in &todo {
            let mut s = it(id, 1, None);
            s.txt = Some("é".repeat(50_000)); // 2 bytes in UTF-8, 1 unit in UTF-16
            b.pending.insert(id.clone(), s);
        }
        assert_eq!(b.bite(&mut todo).len(), 3);
        assert_eq!(b.bite(&mut todo).len(), 2);

        // a viewer forgets its pending items
        let mut b = Board::default();
        b.pending.insert("x".into(), it("x", 1, None));
        b.undo_queue.push("x".into());
        assert_eq!(b.ready(None), Ready::default());
        assert!(b.pending.is_empty() && b.undo_queue.is_empty());
    }

    #[test]
    fn a_sheet_comes_back_under_what_is_left_of_mine() {
        let mut b = Board::default();
        b.insert(it("a", 1, Some(1.0)));
        b.insert(it("m", 2, None));
        b.insert(it("b", 1, Some(2.0)));
        b.pending.insert("m".into(), b.get("m").unwrap().clone());
        assert_eq!(b.clear_sheet(), ["a", "b"]);
        assert_eq!((order(&b), b.count(1), b.count(2)), (vec!["m"], 0, 1));
        assert!(!b.has("a") && b.has("m"));

        let back = vec![it("y", 3, Some(9.0)), it("m", 2, Some(0.5)), it("x", 3, Some(4.0))];
        let layers = vec![LayerInfo { l: 3, c: "#00aa00".into(), del: true, n: None }];
        b.uncrumpled(back, Some(layers));
        assert_eq!(order(&b), ["x", "y", "m"], "the sheet in q order, mine on top, not doubled");
        assert_eq!((b.count(2), b.count(3)), (1, 2));
        assert!(b.layers[&3].del);
        b.uncrumpled(vec![], None);
        assert_eq!(order(&b), ["x", "y", "m"]);
    }
}
