//! The wire protocol (PROTOCOL.md v1), both ways.
//!
//! Incoming frames are read as loosely as the JavaScript page read them: a field of the wrong
//! type becomes its default instead of refusing the frame, because the page has always shown
//! what it could. Outgoing frames are written with the keys in the order the JavaScript page
//! wrote them, and through [`crate::json`], so the bytes on the wire are unchanged.

use crate::json::J;
use crate::shape::Shape;
use serde_json::Value;

/// A layer number. The relay numbers layers from 1.
pub type Layer = i64;

/// A finished stroke or text annotation, as it arrives in `ss`, `e`, `lr`, `un` and `sv`, or as
/// this browser keeps it. `q` is `None` until the relay has stored it.
#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    pub id: String,
    pub l: Option<Layer>,
    pub q: Option<f64>,
    pub c: String,
    pub w: f64,
    pub pts: Vec<f64>,
    pub txt: Option<String>,
    /// A shape or line (A-166, `sh`); never with `txt`.
    pub sh: Option<Shape>,
}

impl Item {
    /// `strokeFrom(m)`: the item fields of a frame or a stored item. `None` without an `id`.
    pub fn from_value(m: &Value) -> Option<Item> {
        Some(Item {
            id: m.get("id")?.as_str()?.to_string(),
            l: m.get("l").and_then(Value::as_i64),
            q: m.get("q").and_then(Value::as_f64),
            c: str_of(m, "c").unwrap_or_default(),
            w: m.get("w").and_then(Value::as_f64).unwrap_or(0.0),
            pts: nums(m.get("pts")).unwrap_or_default(),
            txt: str_of(m, "txt"),
            sh: m.get("sh").and_then(Shape::from_value),
        })
    }

    /// Whether this is a text annotation.
    pub fn is_text(&self) -> bool {
        self.txt.is_some()
    }

    /// `itemOf(s)`: how a private board keeps an item, always on layer 1.
    pub fn stored(&self) -> J {
        let mut f = vec![
            ("id", J::str(&self.id)),
            ("l", J::Num(1.0)),
            ("c", J::str(&self.c)),
            ("w", J::Num(self.w)),
            ("pts", J::nums(&self.pts)),
        ];
        if let Some(t) = &self.txt {
            f.push(("txt", J::str(t)));
        }
        if let Some(sh) = &self.sh {
            f.push(("sh", sh.to_json()));
        }
        J::obj(f)
    }

    /// Whether this is a shape or a line.
    pub fn is_shape(&self) -> bool {
        self.sh.is_some()
    }
}

/// A layer as `hello` and `un` list it.
#[derive(Clone, Debug, PartialEq)]
pub struct LayerInfo {
    pub l: Layer,
    pub c: String,
    pub del: bool,
    pub n: Option<String>,
}

impl LayerInfo {
    fn from_value(v: &Value) -> Option<LayerInfo> {
        Some(LayerInfo {
            l: v.get("l")?.as_i64()?,
            c: str_of(v, "c").unwrap_or_default(),
            del: truthy(v.get("del")),
            // `L.n || null`: an empty name is no name
            n: str_of(v, "n").filter(|n| !n.is_empty()),
        })
    }
}

/// A crumpled sheet on the shelf: its sheet number and how many items it holds.
#[derive(Clone, Debug, PartialEq)]
pub struct ShelfEntry {
    pub k: i64,
    pub n: i64,
}

impl ShelfEntry {
    fn from_value(v: &Value) -> Option<ShelfEntry> {
        Some(ShelfEntry { k: v.get("k")?.as_i64()?, n: v.get("n").and_then(Value::as_i64).unwrap_or(0) })
    }
}

/// A frame from the relay.
#[derive(Clone, Debug, PartialEq)]
pub enum In {
    Hello { edit: bool, me: Option<Layer>, view: Option<String>, layers: Vec<LayerInfo>, shelf: Vec<ShelfEntry> },
    Snapshot(Vec<Item>),
    Ready,
    Begin { id: String, l: Option<Layer>, c: String, w: f64 },
    /// `pts` is `None` when the field is not an array: the JavaScript page ignored the frame.
    Points { id: String, pts: Option<Vec<f64>> },
    End(Item),
    Ok { id: String, q: Option<f64> },
    No { id: String, m: Option<String> },
    Undo { id: String },
    NewLayer { l: Layer, c: String },
    LayerDeleted { l: Layer },
    LayerRestored { l: Layer, a: Option<Vec<Item>> },
    Crumpled { k: i64, by: Option<Layer>, drop: Option<i64>, shelf: Vec<ShelfEntry> },
    Uncrumpled { k: i64, by: Option<Layer>, shelf: Vec<ShelfEntry>, layers: Option<Vec<LayerInfo>>, a: Vec<Item> },
    SheetView { k: i64, a: Option<Vec<Item>> },
    People { n: i64, on: Vec<Layer> },
    Name { l: Layer, n: Option<String>, c: Option<String> },
    Ping { l: Option<Layer>, k: String, pts: Option<Vec<f64>> },
    Error { code: String, m: Option<String> },
}

impl In {
    /// One text frame. `None` for a frame the page ignores: not JSON, not an object, or an
    /// unknown `t`.
    pub fn parse(text: &str) -> Option<In> {
        let m: Value = serde_json::from_str(text).ok()?;
        if !m.is_object() {
            return None;
        }
        let id = || str_of(&m, "id").unwrap_or_default();
        let l = || m.get("l").and_then(Value::as_i64);
        let k = || m.get("k").and_then(Value::as_i64).unwrap_or(0);
        Some(match m.get("t")?.as_str()? {
            "hello" => In::Hello {
                edit: m.get("role").and_then(Value::as_str) == Some("edit"),
                me: m.get("me").and_then(Value::as_i64),
                view: str_of(&m, "view").filter(|v| !v.is_empty()),
                layers: list(m.get("layers"), LayerInfo::from_value).unwrap_or_default(),
                shelf: list(m.get("shelf"), ShelfEntry::from_value).unwrap_or_default(),
            },
            "ss" => In::Snapshot(items(m.get("a")).unwrap_or_default()),
            "ready" => In::Ready,
            "b" => In::Begin {
                id: id(),
                l: l(),
                c: str_of(&m, "c").unwrap_or_default(),
                w: m.get("w").and_then(Value::as_f64).unwrap_or(0.0),
            },
            "p" => In::Points { id: id(), pts: nums(m.get("pts")) },
            "e" => In::End(Item::from_value(&m)?),
            "ok" => In::Ok { id: id(), q: m.get("q").and_then(Value::as_f64) },
            "no" => In::No { id: id(), m: str_of(&m, "m").filter(|s| !s.is_empty()) },
            "u" => In::Undo { id: id() },
            "l" => In::NewLayer { l: l()?, c: str_of(&m, "c").unwrap_or_default() },
            "ld" => In::LayerDeleted { l: l()? },
            "lr" => In::LayerRestored { l: l()?, a: items(m.get("a")) },
            "cr" => In::Crumpled {
                k: k(),
                by: m.get("by").and_then(Value::as_i64),
                drop: m.get("drop").and_then(Value::as_i64),
                shelf: list(m.get("shelf"), ShelfEntry::from_value).unwrap_or_default(),
            },
            "un" => In::Uncrumpled {
                k: k(),
                by: m.get("by").and_then(Value::as_i64),
                shelf: list(m.get("shelf"), ShelfEntry::from_value).unwrap_or_default(),
                layers: list(m.get("layers"), LayerInfo::from_value),
                a: items(m.get("a")).unwrap_or_default(),
            },
            "sv" => In::SheetView { k: k(), a: items(m.get("a")) },
            "n" => In::People {
                // `m.n | 0`
                n: m.get("n").and_then(Value::as_f64).map_or(0, |v| v as i32 as i64),
                on: m
                    .get("on")
                    .and_then(Value::as_array)
                    .map(|a| a.iter().filter_map(Value::as_i64).collect())
                    .unwrap_or_default(),
            },
            "nm" => In::Name { l: l()?, n: str_of(&m, "n"), c: str_of(&m, "c") },
            "pg" => In::Ping {
                l: l(),
                // `String(m.k || '?').slice(0, 1)`
                k: str_of(&m, "k")
                    .filter(|s| !s.is_empty())
                    .and_then(|s| s.chars().next())
                    .unwrap_or('?')
                    .to_string(),
                pts: nums(m.get("pts")),
            },
            "err" => In::Error { code: str_of(&m, "code").unwrap_or_default(), m: str_of(&m, "m").filter(|s| !s.is_empty()) },
            _ => return None,
        })
    }
}

/// A frame to the relay.
#[derive(Clone, Debug, PartialEq)]
pub enum Out {
    /// `who` is the contributor secret, which the page always has (it makes one when it has none).
    Join { key: String, who: String },
    Begin { id: String, c: String, w: f64 },
    Points { id: String, pts: Vec<f64> },
    End { id: String, c: String, w: f64, pts: Vec<f64> },
    Stroke { id: String, c: String, w: f64, pts: Vec<f64>, txt: Option<String>, sh: Option<Shape> },
    Undo { id: String },
    LayerDelete { l: Layer },
    LayerRestore { l: Layer },
    Crumple,
    Uncrumple { k: i64 },
    SheetView { k: i64 },
    Name { n: String, c: String },
    Ping { pts: Vec<f64> },
}

impl Out {
    pub fn to_json(&self) -> J {
        let t = |t: &str| ("t", J::str(t));
        match self {
            Out::Join { key, who } => J::obj([
                t("join"),
                ("key", J::str(key)),
                ("who", J::str(who)),
            ]),
            Out::Begin { id, c, w } => J::obj([t("b"), ("id", J::str(id)), ("c", J::str(c)), ("w", J::Num(*w))]),
            Out::Points { id, pts } => J::obj([t("p"), ("id", J::str(id)), ("pts", J::nums(pts))]),
            Out::End { id, c, w, pts } => J::obj([
                t("e"),
                ("id", J::str(id)),
                ("c", J::str(c)),
                ("w", J::Num(*w)),
                ("pts", J::nums(pts)),
            ]),
            Out::Stroke { id, c, w, pts, txt, sh } => {
                let mut f =
                    vec![t("s"), ("id", J::str(id)), ("c", J::str(c)), ("w", J::Num(*w)), ("pts", J::nums(pts))];
                if let Some(txt) = txt {
                    f.push(("txt", J::str(txt)));
                }
                if let Some(sh) = sh {
                    f.push(("sh", sh.to_json()));
                }
                J::obj(f)
            }
            Out::Undo { id } => J::obj([t("u"), ("id", J::str(id))]),
            Out::LayerDelete { l } => J::obj([t("ld"), ("l", J::Num(*l as f64))]),
            Out::LayerRestore { l } => J::obj([t("lr"), ("l", J::Num(*l as f64))]),
            Out::Crumple => J::obj([t("cr")]),
            Out::Uncrumple { k } => J::obj([t("un"), ("k", J::Num(*k as f64))]),
            Out::SheetView { k } => J::obj([t("sv"), ("k", J::Num(*k as f64))]),
            Out::Name { n, c } => J::obj([t("nm"), ("n", J::str(n)), ("c", J::str(c))]),
            Out::Ping { pts } => J::obj([t("pg"), ("pts", J::nums(pts))]),
        }
    }

    /// The text frame.
    pub fn frame(&self) -> String {
        self.to_json().stringify()
    }
}

/// `typeof v[k] === 'string' ? v[k] : undefined`.
pub fn str_of(v: &Value, k: &str) -> Option<String> {
    v.get(k).and_then(Value::as_str).map(str::to_string)
}

/// The numbers of an array, or `None` when it is not an array. A non-number in the array is
/// read as 0 (the JavaScript page would have drawn with NaN and shown nothing; 0 at least keeps
/// the indices of the triples).
pub fn nums(v: Option<&Value>) -> Option<Vec<f64>> {
    Some(v?.as_array()?.iter().map(|x| x.as_f64().unwrap_or(0.0)).collect())
}

fn list<T>(v: Option<&Value>, f: impl Fn(&Value) -> Option<T>) -> Option<Vec<T>> {
    Some(v?.as_array()?.iter().filter_map(f).collect())
}

/// An item list, skipping entries without an `id`.
pub fn items(v: Option<&Value>) -> Option<Vec<Item>> {
    list(v, Item::from_value)
}

/// JavaScript truthiness of a JSON value.
pub fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|x| x != 0.0 && !x.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A number read off the wire is the double JSON.parse gives, so it is written back the
    /// same. Without serde_json's float_roundtrip, 3.9907793928864868 read as ...863.
    #[test]
    fn numbers_read_back_exactly() {
        for t in ["3.9907793928864868", "0.2006145653048538", "1.7976931348623157e+308", "5e-324", "0.1"] {
            let frame = format!(r#"{{"t":"p","id":"a","pts":[{t}]}}"#);
            let Some(In::Points { pts: Some(p), .. }) = In::parse(&frame) else { panic!("{frame}") };
            assert_eq!(p[0], t.parse::<f64>().unwrap(), "{t}");
            assert_eq!(crate::json::num_to_string(p[0]), t);
        }
    }

    /// Each outgoing frame, as the JavaScript page's `JSON.stringify` wrote it.
    #[test]
    fn outgoing_frames_match_the_javascript_page_byte_for_byte() {
        let s = |x: &str| x.to_string();
        let cases = [
            (Out::Join { key: s("K"), who: s("W") }, r#"{"t":"join","key":"K","who":"W"}"#),
            (Out::Begin { id: s("a1"), c: s("#111111"), w: 0.004 }, r##"{"t":"b","id":"a1","c":"#111111","w":0.004}"##),
            (Out::Points { id: s("a1"), pts: vec![0.0, 0.5, 1.0] }, r#"{"t":"p","id":"a1","pts":[0,0.5,1]}"#),
            (
                Out::End { id: s("a1"), c: s("#111111"), w: 0.024, pts: vec![0.12345, -0.5, 0.5] },
                r##"{"t":"e","id":"a1","c":"#111111","w":0.024,"pts":[0.12345,-0.5,0.5]}"##,
            ),
            (
                Out::Stroke { id: s("a1"), c: s("#d63031"), w: 0.002, pts: vec![1.0, 1.0, 0.5], txt: None, sh: None },
                r##"{"t":"s","id":"a1","c":"#d63031","w":0.002,"pts":[1,1,0.5]}"##,
            ),
            (
                Out::Stroke { id: s("t1"), c: s("#d63031"), w: 0.02, pts: vec![0.0, 0.0, 0.5], txt: Some(s("hi\nthere")), sh: None },
                r##"{"t":"s","id":"t1","c":"#d63031","w":0.02,"pts":[0,0,0.5],"txt":"hi\nthere"}"##,
            ),
            (Out::Undo { id: s("a1") }, r#"{"t":"u","id":"a1"}"#),
            (Out::LayerDelete { l: 3 }, r#"{"t":"ld","l":3}"#),
            (Out::LayerRestore { l: 3 }, r#"{"t":"lr","l":3}"#),
            (Out::Crumple, r#"{"t":"cr"}"#),
            (Out::Uncrumple { k: 7 }, r#"{"t":"un","k":7}"#),
            (Out::SheetView { k: 7 }, r#"{"t":"sv","k":7}"#),
            (Out::Name { n: s("Blue Otter"), c: s("#2069c4") }, r##"{"t":"nm","n":"Blue Otter","c":"#2069c4"}"##),
            (Out::Ping { pts: vec![0.1, 0.2, 0.5] }, r#"{"t":"pg","pts":[0.1,0.2,0.5]}"#),
        ];
        for (m, want) in cases {
            assert_eq!(m.frame(), want);
        }
    }

    #[test]
    fn a_hello_reads_its_layers_and_shelf() {
        let m = In::parse(
            r##"{"t":"hello","role":"edit","me":2,"view":"V","layers":[{"l":1,"c":"#111111","del":false,"n":null},{"l":2,"c":"#d63031","del":true,"n":"Ann"}],"shelf":[{"k":4,"n":12}]}"##,
        );
        assert_eq!(
            m,
            Some(In::Hello {
                edit: true,
                me: Some(2),
                view: Some("V".into()),
                layers: vec![
                    LayerInfo { l: 1, c: "#111111".into(), del: false, n: None },
                    LayerInfo { l: 2, c: "#d63031".into(), del: true, n: Some("Ann".into()) },
                ],
                shelf: vec![ShelfEntry { k: 4, n: 12 }],
            })
        );
        let v = In::parse(r#"{"t":"hello","role":"view","me":null,"view":null,"layers":[],"shelf":[]}"#);
        assert_eq!(v, Some(In::Hello { edit: false, me: None, view: None, layers: vec![], shelf: vec![] }));
    }

    #[test]
    fn items_read_as_stroke_from_reads_them() {
        let m = In::parse(r##"{"t":"e","id":"x","l":3,"q":17,"c":"#0a66d8","w":0.004,"pts":[0,0,0.5,1,1,0.5]}"##);
        assert_eq!(
            m,
            Some(In::End(Item {
                id: "x".into(),
                l: Some(3),
                q: Some(17.0),
                c: "#0a66d8".into(),
                w: 0.004,
                pts: vec![0.0, 0.0, 0.5, 1.0, 1.0, 0.5],
                txt: None,
                sh: None,
            }))
        );
        // a text annotation, and a missing `pts` read as empty
        let Some(In::Snapshot(a)) = In::parse(r##"{"t":"ss","a":[{"id":"t","l":1,"q":1,"c":"#111111","w":0.02,"pts":[0,0,0.5],"txt":"hi"},{"id":"y","l":1}]}"##) else {
            panic!()
        };
        assert_eq!(a[0].txt.as_deref(), Some("hi"));
        assert!(a[1].pts.is_empty() && a[1].q.is_none());
    }

    #[test]
    fn odd_frames_are_read_as_the_javascript_page_read_them() {
        assert_eq!(In::parse("not json"), None);
        assert_eq!(In::parse("[1,2]"), None);
        assert_eq!(In::parse(r#"{"t":"zz"}"#), None);
        assert_eq!(In::parse(r#"{"t":"n","n":3.9,"on":[1,"x",2]}"#), Some(In::People { n: 3, on: vec![1, 2] }));
        assert_eq!(In::parse(r#"{"t":"n"}"#), Some(In::People { n: 0, on: vec![] }));
        assert_eq!(In::parse(r#"{"t":"p","id":"a","pts":5}"#), Some(In::Points { id: "a".into(), pts: None }));
        assert_eq!(
            In::parse(r#"{"t":"pg","l":2,"k":"QX","pts":[]}"#),
            Some(In::Ping { l: Some(2), k: "Q".into(), pts: Some(vec![]) })
        );
        assert_eq!(In::parse(r#"{"t":"pg","l":2,"k":"","pts":[]}"#), Some(In::Ping { l: Some(2), k: "?".into(), pts: Some(vec![]) }));
        assert_eq!(In::parse(r#"{"t":"no","id":"a","m":""}"#), Some(In::No { id: "a".into(), m: None }));
    }

    #[test]
    fn a_stored_item_is_on_layer_one_and_keeps_its_text_only_when_it_has_one() {
        let mut it = Item { id: "a".into(), l: Some(4), q: Some(9.0), c: "#111111".into(), w: 0.004, pts: vec![0.0, 1.0, 0.5], txt: None, sh: None };
        assert_eq!(it.stored().stringify(), r##"{"id":"a","l":1,"c":"#111111","w":0.004,"pts":[0,1,0.5]}"##);
        it.txt = Some("x".into());
        assert_eq!(it.stored().stringify(), r##"{"id":"a","l":1,"c":"#111111","w":0.004,"pts":[0,1,0.5],"txt":"x"}"##);
    }
}
