//! The private board: a board that lives only in this browser until it is shared, and the
//! other things the page keeps in `localStorage`.
//!
//! The stored bytes must stay what the JavaScript page wrote, so a board drawn before the
//! switch to Rust loads after it, and the other way round. `wb.local` is
//! `JSON.stringify({ v: 1, a, shelf, k })`; this module writes it through [`crate::json`].
//!
//! Nothing here touches the browser. The caller reads and writes the strings; this module
//! decides what they hold.

use crate::json::J;
use crate::model::Board;
use crate::proto::{items, In, Item, Layer, ShelfEntry};
use indexmap::IndexSet;
use serde_json::Value;

/// Where the private board is kept.
pub const LOCAL_KEY: &str = "wb.local";
/// While a private board is being shared, the edit key of the board it is going to. A reload
/// before the upload finished resumes it there ([`sharing_target`]) instead of making a
/// second board on the next Share.
pub const SHARING_KEY: &str = "wb.sharing";
/// How many crumpled sheets the shelf keeps, as the relay does.
pub const SHELF_MAX: usize = 5;

/// What [`SHARING_KEY`] holds, if it is a key a link can carry.
pub fn sharing_target(stored: Option<&str>) -> Option<&str> {
    stored.filter(|k| valid_secret(k))
}

/// The `localStorage` key for my contributor secret on board `key`.
pub fn who_key(key: &str) -> String {
    format!("wb:who:{key}")
}

/// The `localStorage` key for the layers I hide on board `key`.
pub fn hide_key(key: &str) -> String {
    format!("wb:hide:{key}")
}

/// `/^[A-Za-z0-9_-]{16,64}$/`: whether a stored contributor secret can be used as it is.
pub fn valid_secret(s: &str) -> bool {
    (16..=64).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// A new contributor secret from 24 random bytes: base64url, no padding (32 characters).
pub fn secret_from(bytes: &[u8; 24]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut s = String::with_capacity(32);
    for c in bytes.chunks(3) {
        let n = (c[0] as u32) << 16 | (c[1] as u32) << 8 | c[2] as u32;
        for i in (0..4).rev() {
            s.push(A[(n >> (6 * i) & 63) as usize] as char);
        }
    }
    s
}

/// `loadHidden`: the layers I hide, in the order they were hidden. A bad value is no value.
pub fn parse_hidden(stored: Option<&str>) -> IndexSet<Layer> {
    let mut out = IndexSet::new();
    // `lsGet(..) || '[]'`: an empty string is no value too
    let text = stored.filter(|s| !s.is_empty()).unwrap_or("[]");
    if let Ok(Value::Array(a)) = serde_json::from_str::<Value>(text) {
        for v in &a {
            // `Number.isInteger`. Layer numbers past 2^53 are not layer numbers; they are skipped.
            if let Some(x) = v.as_f64() {
                if x.fract() == 0.0 && x.abs() <= 9_007_199_254_740_992.0 {
                    out.insert(x as Layer);
                }
            }
        }
    }
    out
}

/// `saveHidden`.
pub fn hidden_json(hidden: &IndexSet<Layer>) -> String {
    J::Arr(hidden.iter().map(|&l| J::Num(l as f64)).collect()).stringify()
}

/// A crumpled sheet kept in this browser, with its items.
#[derive(Clone, Debug, PartialEq)]
pub struct Sheet {
    pub k: i64,
    pub a: Vec<Item>,
}

/// What became of a save.
#[derive(Clone, Debug, PartialEq)]
pub enum Save {
    /// Nothing changed since the last save.
    Unchanged,
    /// Write this to [`LOCAL_KEY`], then call [`Local::saved`] with whether it was written.
    Write { sig: String, text: String },
    /// Another tab wrote the private board since this one last read or wrote it: writing
    /// now would throw its work away. Load it again instead ([`Local::load`]).
    Elsewhere,
    /// What is stored could not be read ([`Local::damaged`]). Writing would replace it with
    /// this board, so nothing is written until the user keeps or discards it.
    Damaged,
}

/// `text` with every unpaired surrogate escape (`\ud83d` with no low half after it, or a
/// lone low half) replaced by `\ufffd`. `JSON.stringify` writes them and `JSON.parse` reads
/// them, but they are not Unicode, so serde_json refuses the whole document.
fn mend_surrogates(text: &str) -> std::borrow::Cow<'_, str> {
    if !text.contains("\\u") {
        return text.into();
    }
    fn unit(b: &[u8], i: usize) -> Option<u16> {
        if b.get(i) != Some(&b'\\') || b.get(i + 1) != Some(&b'u') {
            return None;
        }
        u16::from_str_radix(std::str::from_utf8(b.get(i + 2..i + 6)?).ok()?, 16).ok()
    }
    let b = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let (mut i, mut from) = (0, 0);
    while i < b.len() {
        if b[i] != b'\\' {
            i += 1;
            continue;
        }
        // in JSON a backslash only starts an escape, so pairs are consumed from the left
        let Some(u) = unit(b, i) else {
            i += 2;
            continue;
        };
        let paired = (0xd800..0xdc00).contains(&u) && unit(b, i + 6).is_some_and(|l| (0xdc00..0xe000).contains(&l));
        if paired {
            i += 12;
        } else if (0xd800..0xe000).contains(&u) {
            out.push_str(&text[from..i]);
            out.push_str("\\ufffd");
            i += 6;
            from = i;
        } else {
            i += 6;
        }
    }
    if from == 0 {
        return text.into();
    }
    out.push_str(&text[from..]);
    out.into()
}

/// What a tab knows of the stored text: a hash of it, or `None` for nothing stored. FNV-1a,
/// so it is the same on every build.
fn stamp(stored: Option<&str>) -> Option<u64> {
    stored.map(|t| t.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3)))
}

/// The private board's own state: the shelf and the next sheet number. The items are on the
/// [`Board`], as they were in `strokes`.
#[derive(Clone, Debug, PartialEq)]
pub struct Local {
    pub shelf: Vec<Sheet>,
    pub next_k: i64,
    saved_sig: String,
    /// The stored text as this tab last read or wrote it ([`stamp`]).
    seen: Option<u64>,
    /// Something was stored that could not be read; see [`Local::damaged`].
    damaged: bool,
}

impl Default for Local {
    fn default() -> Local {
        Local { shelf: vec![], next_k: 1, saved_sig: String::new(), seen: None, damaged: false }
    }
}

/// `x | 0`: JavaScript's ToInt32 of a JSON value (`undefined`, a string or an object is 0).
fn to_int32(v: Option<&Value>) -> i64 {
    let x = match v {
        Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0),
        Some(Value::Bool(true)) => 1.0,
        _ => return 0, // a numeric string would count in JavaScript; the page never wrote one
    };
    if !x.is_finite() {
        return 0;
    }
    let m = x.trunc().rem_euclid(4_294_967_296.0);
    (if m >= 2_147_483_648.0 { m - 4_294_967_296.0 } else { m }) as i64
}

/// `itemOf` for a list.
fn stored_items(a: &[Item]) -> J {
    J::Arr(a.iter().map(Item::stored).collect())
}

impl Local {
    /// `loadLocal`, into an empty board. Returns the state and the ids loaded, oldest first,
    /// which are mine to undo. Every loaded item is pending: it goes to the relay on share.
    ///
    /// A stored item without a string `id` or an array `pts`, or with an id already loaded, is
    /// skipped, as the JavaScript page skipped it. A field of the wrong type in a kept item
    /// becomes its default ([`Item::from_value`]); the JavaScript page never wrote one.
    ///
    /// Stored text that is not the page's `{ v: 1, a: [...] }` loads nothing and leaves the
    /// state [`damaged`](Local::damaged), where the JavaScript page started a blank board and
    /// wrote it over what was there.
    pub fn load(stored: Option<&str>, board: &mut Board) -> (Local, Vec<String>) {
        let mut st = Local { seen: stamp(stored), ..Local::default() };
        let mut mine = vec![];
        let Some(text) = stored.filter(|s| !s.is_empty()) else { return (st, mine) };
        let d = serde_json::from_str::<Value>(&mend_surrogates(text)).ok();
        let a = d.as_ref().filter(|d| d.get("v").and_then(Value::as_f64) == Some(1.0)).and_then(|d| d.get("a")?.as_array());
        let (Some(d), Some(a)) = (d.as_ref(), a) else {
            st.damaged = true;
            return (st, mine);
        };
        for raw in a {
            if !raw.get("pts").is_some_and(Value::is_array) || raw.get("id").and_then(Value::as_str).is_none() {
                continue;
            }
            let Some(mut s) = Item::from_value(raw) else { continue };
            if board.has(&s.id) {
                continue;
            }
            s.l = Some(1);
            s.q = None;
            board.pending.insert(s.id.clone(), s.clone());
            mine.push(s.id.clone());
            board.insert(s); // no q: it goes last, as `strokes.push` put it
        }
        if let Some(shelf) = d.get("shelf").and_then(Value::as_array) {
            // `h && Array.isArray(h.a)`, then the last five. Only what the page wrote round-trips:
            // anything else (a sheet with no integer k, items that do not parse) is dropped, not
            // coerced.
            let kept: Vec<Sheet> = shelf
                .iter()
                .filter_map(|h| {
                    let a = items(Some(h.get("a").filter(|a| a.is_array())?))?;
                    Some(Sheet { k: h.get("k")?.as_i64()?, a })
                })
                .collect();
            let n = kept.len();
            st.shelf = kept.into_iter().skip(n.saturating_sub(SHELF_MAX)).collect();
        }
        st.next_k = st.shelf.iter().map(|h| h.k + 1).fold(to_int32(d.get("k")).max(1), i64::max);
        (st, mine)
    }

    /// What [`LOCAL_KEY`] holds once a share has finished: only `kept`, the items the relay
    /// refused (usually none; the rest are on the relay now), with the same shelf and sheet
    /// number, because crumpled sheets are not shared. `stored` is what [`LOCAL_KEY`] holds
    /// now. `None` when writing would throw work away, the same guards as [`Local::save`]: the
    /// stored board could not be read (F1), or another tab wrote it since this one last read
    /// or wrote it (council H3).
    pub fn after_share(&self, kept: &Board, stored: Option<&str>) -> Option<String> {
        if self.damaged || self.elsewhere(stored) {
            return None;
        }
        Some(self.text(kept))
    }

    /// `localShelfList`: the shelf as the relay lists it.
    pub fn shelf_list(&self) -> Vec<ShelfEntry> {
        self.shelf.iter().map(|h| ShelfEntry { k: h.k, n: h.a.len() as i64 }).collect()
    }

    /// The save signature: the item count, the last item's id and the sheet numbers. A change
    /// to any of them is a change worth writing.
    fn sig(&self, board: &Board) -> String {
        let last = board.strokes.last().map_or("", |s| s.id.as_str());
        let ks: Vec<String> = self.shelf.iter().map(|h| h.k.to_string()).collect();
        format!("{}:{}:{}", board.strokes.len(), last, ks.join(","))
    }

    /// The stored text: `JSON.stringify({ v: 1, a: strokes.map(itemOf), shelf, k })`.
    pub fn text(&self, board: &Board) -> String {
        let shelf = J::Arr(
            self.shelf.iter().map(|h| J::obj([("k", J::Num(h.k as f64)), ("a", stored_items(&h.a))])).collect(),
        );
        J::obj([
            ("v", J::Num(1.0)),
            ("a", stored_items(&board.strokes)),
            ("shelf", shelf),
            ("k", J::Num(self.next_k as f64)),
        ])
        .stringify()
    }

    /// `saveLocal`, first half: what to write, if anything. `stored` is what [`LOCAL_KEY`]
    /// holds now. Two tabs on `/` share it; one that wrote since this one last read it wins,
    /// and this one is told to load again rather than write over it.
    pub fn save(&self, board: &Board, stored: Option<&str>) -> Save {
        if self.damaged {
            return Save::Damaged;
        }
        let sig = self.sig(board);
        if sig == self.saved_sig {
            return Save::Unchanged;
        }
        if self.elsewhere(stored) {
            return Save::Elsewhere;
        }
        Save::Write { text: self.text(board), sig }
    }

    /// Whether what is stored could not be read when this tab loaded it. Until
    /// [`Local::discard_damaged`] nothing is saved, so the stored text stays as it was for the
    /// user to keep (the caller offers it as a download) or discard.
    pub fn damaged(&self) -> bool {
        self.damaged
    }

    /// The user chose to start over: the next save writes this board over what was stored.
    pub fn discard_damaged(&mut self) {
        self.damaged = false;
    }

    /// Whether `stored` (what [`LOCAL_KEY`] holds now, say on a `storage` event) was written
    /// by another tab since this one last read or wrote it.
    pub fn elsewhere(&self, stored: Option<&str>) -> bool {
        stamp(stored) != self.seen
    }

    /// `saveLocal`, second half: `text` was written (`ok`) or refused (the store is full).
    /// `true` when the "too big to keep" toast should show, which is once until a save works.
    pub fn saved(&mut self, sig: String, text: &str, ok: bool) -> bool {
        if ok {
            self.saved_sig = sig;
            self.seen = stamp(Some(text));
            return false;
        }
        let first = self.saved_sig != "full";
        self.saved_sig = "full".into();
        first
    }

    /// `cr`, done here instead of by the relay: the board goes on the shelf. The caller clears
    /// `mine`, handles the returned frame as if the relay had sent it, then calls
    /// [`Local::save`] (`localSend` ends every branch with `saveLocal`). A blank board is not
    /// shelved and nothing changes (`None`), as the relay ignores `cr` on a blank board.
    pub fn crumple(&mut self, board: &mut Board) -> Option<In> {
        if board.strokes.is_empty() {
            return None;
        }
        let k = self.next_k;
        self.next_k += 1;
        self.shelf.push(Sheet { k, a: board.strokes.iter().map(item_of).collect() });
        let drop = if self.shelf.len() > SHELF_MAX { Some(self.shelf.remove(0).k) } else { None };
        board.pending.clear();
        Some(In::Crumpled { k, by: Some(1), drop, shelf: self.shelf_list() })
    }

    /// `un`: sheet `k` comes back off the shelf, and the board as it is goes on it (when it has
    /// anything). `None` when there is no sheet `k`. The caller clears `mine`, handles the frame,
    /// then calls [`Repend::apply`] and [`Local::save`].
    pub fn uncrumple(&mut self, board: &mut Board, k: i64) -> Option<(In, Repend)> {
        let i = self.shelf.iter().position(|h| h.k == k)?;
        let sheet = self.shelf.remove(i);
        if !board.strokes.is_empty() {
            self.shelf.push(Sheet { k: self.next_k, a: board.strokes.iter().map(item_of).collect() });
            self.next_k += 1;
            if self.shelf.len() > SHELF_MAX {
                self.shelf.remove(0);
            }
        }
        board.pending.clear();
        Some((In::Uncrumpled { k, by: Some(1), shelf: self.shelf_list(), layers: None, a: sheet.a }, Repend(())))
    }

    /// `sv`: a look at sheet `k`. `None` when there is no sheet `k`. The caller handles the
    /// frame, then calls [`Local::save`].
    pub fn sheet_view(&self, k: i64) -> Option<In> {
        let h = self.shelf.iter().find(|h| h.k == k)?;
        Some(In::SheetView { k, a: Some(h.a.clone()) })
    }
}

/// `itemOf`: an item as the private board keeps it, in layer 1 with no `q`.
pub fn item_of(s: &Item) -> Item {
    Item { id: s.id.clone(), l: Some(1), q: None, c: s.c.clone(), w: s.w, pts: s.pts.clone(), txt: s.txt.clone() }
}

/// The step an `un` owes once its frame is handled: every item on the board is unconfirmed
/// again and goes to the relay on share. Not applying it leaves the restored sheet out of a
/// later share, so it must be used.
#[must_use = "apply it once the Uncrumpled frame is handled"]
#[derive(Debug, PartialEq, Eq)]
pub struct Repend(());

impl Repend {
    pub fn apply(self, board: &mut Board) {
        for s in &mut board.strokes {
            s.q = None;
            board.pending.insert(s.id.clone(), s.clone());
        }
    }
}

/// What the address asks for.
#[derive(Clone, Debug, PartialEq)]
pub enum Route {
    /// `/b/<key>`: a shared board.
    Board(String),
    /// `/` or `/index.html`: a new private board.
    Local,
    /// Anything else: "This board doesn't exist or has expired."
    Missing,
}

/// `boot`'s reading of `location.pathname`.
pub fn route(path: &str) -> Route {
    if path == "/" || path == "/index.html" {
        return Route::Local;
    }
    if let Some(rest) = path.strip_prefix("/b/") {
        let key = rest.strip_suffix('/').unwrap_or(rest);
        if (1..=64).contains(&key.len()) && key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-') {
            return Route::Board(key.to_string());
        }
    }
    Route::Missing
}

#[cfg(test)]
mod tests {
    use super::*;

    fn it(id: &str) -> Item {
        Item { id: id.into(), l: Some(1), q: None, c: "#111111".into(), w: 3.0, pts: vec![0.1, 0.2, 0.5], txt: None }
    }

    fn board(ids: &[&str]) -> Board {
        let mut b = Board::default();
        for id in ids {
            b.insert(it(id));
        }
        b
    }

    /// Written by the JavaScript page (node, `JSON.stringify` of `itemOf`s): awkward numbers,
    /// a text item with quotes, a newline, a control character and non-ASCII, and a full shelf.
    const JS_WROTE: &str = concat!(
        r##"{"v":1,"a":[{"id":"a1","l":1,"c":"#e04a3f","w":4.5,"pts":[0.1,-0.30000000000000004,1e-7,123456789012345680000,0.5]},"##,
        r##"{"id":"t2","l":1,"c":"#111111","w":0.0123,"pts":[-0.5,0.25,0.5],"txt":"say \"hi\"\nthen\u0007 ünïcödé ✓"}],"##,
        r##""shelf":[{"k":3,"a":[]},{"k":4,"a":[{"id":"x","l":1,"c":"#2a6","w":2,"pts":[5e-324,1.7976931348623157e+308,0]}]}],"k":5}"##
    );

    #[test]
    fn a_board_the_js_page_wrote_saves_back_byte_for_byte() {
        let mut b = Board::default();
        let (st, mine) = Local::load(Some(JS_WROTE), &mut b);
        assert_eq!(mine, ["a1", "t2"]);
        assert_eq!(b.pending.len(), 2);
        assert_eq!(b.count(1), 2);
        assert_eq!((st.shelf.len(), st.next_k), (2, 5));
        assert_eq!(st.text(&b), JS_WROTE);
    }

    #[test]
    fn loading_skips_what_the_js_page_skipped() {
        let mut b = Board::default();
        let s = r#"{"v":1,"a":[null,7,{"id":1,"pts":[]},{"id":"n"},{"id":"d","pts":[1],"txt":5},{"id":"d","pts":[2]}],
            "shelf":[null,{"k":1},{"k":2,"a":[{"pts":[]},{"id":"y","pts":[]}]}],"k":"9"}"#;
        let (st, mine) = Local::load(Some(s), &mut b);
        assert_eq!(mine, ["d"]);
        assert_eq!(b.strokes[0].pts, [1.0]);
        assert_eq!(b.strokes[0].txt, None, "a txt that is not a string is dropped");
        let y = Item { id: "y".into(), l: None, q: None, c: String::new(), w: 0.0, pts: vec![], txt: None };
        assert_eq!(st.shelf, [Sheet { k: 2, a: vec![y] }]);
        assert_eq!(st.next_k, 3, "Rust reads a string k as 0, where JS would coerce '9' to 9; the page never writes one");
    }

    #[test]
    fn nothing_stored_is_a_blank_board() {
        for s in [None, Some("")] {
            let mut b = Board::default();
            let (st, mine) = Local::load(s, &mut b);
            let blank = Local { seen: stamp(s), ..Local::default() };
            assert_eq!((st, mine.len(), b.strokes.len()), (blank, 0, 0), "{s:?}");
        }
    }

    #[test]
    fn what_cannot_be_read_is_kept_until_the_user_decides() {
        let no_a = r#"{"v":1,"shelf":[{"k":4,"a":[]}],"k":9}"#;
        let no_v = r#"{"a":[{"id":"t","pts":[0,0,0.5]}]}"#;
        let v2 = r#"{"v":2,"a":[]}"#;
        let bad_escape = r#"{"v":1,"a":[{"id":"t","pts":[0],"txt":"\x"}]}"#;
        for s in ["{", "null", "[1]", r#"{"v":1,"a":{}}"#, no_a, no_v, v2, bad_escape] {
            let mut b = Board::default();
            let (mut st, mine) = Local::load(Some(s), &mut b);
            assert!(st.damaged(), "{s}");
            assert_eq!((mine.len(), b.strokes.len(), st.shelf.len()), (0, 0, 0), "{s}");
            b.insert(Item { id: "new".into(), l: Some(1), q: None, c: String::new(), w: 1.0, pts: vec![], txt: None });
            assert_eq!(st.save(&b, Some(s)), Save::Damaged, "{s}: never written over");
            st.discard_damaged();
            assert!(matches!(st.save(&b, Some(s)), Save::Write { .. }), "{s}: the user chose to start over");
        }
    }

    /// Council H3: the last write of a share goes through the save guards. On 5884277 it
    /// wrote over a damaged board (which F1 keeps for the user) and over another tab's newer
    /// board.
    #[test]
    fn a_finished_share_never_writes_over_a_damaged_or_newer_board() {
        let (st, _) = Local::load(Some(JS_WROTE), &mut Board::default());
        assert!(st.after_share(&Board::default(), Some(JS_WROTE)).is_some(), "as this tab left it");
        let newer = r#"{"v":1,"a":[],"shelf":[],"k":9}"#;
        assert_eq!(st.after_share(&Board::default(), Some(newer)), None, "another tab wrote since");
        let broken = r#"{"v":1,"a":[{"broken"#;
        let (st, _) = Local::load(Some(broken), &mut Board::default());
        assert!(st.damaged());
        assert_eq!(st.after_share(&Board::default(), Some(broken)), None, "F1 keeps it");
    }

    #[test]
    fn a_finished_share_keeps_the_shelf_and_a_reload_can_resume_it() {
        let mut b = Board::default();
        let (st, _) = Local::load(Some(JS_WROTE), &mut b);
        let after = st.after_share(&Board::default(), Some(JS_WROTE)).unwrap();
        let (back, mine) = Local::load(Some(&after), &mut Board::default());
        assert!(mine.is_empty(), "the items went to the relay");
        assert_eq!((back.shelf, back.next_k), (st.shelf, 5), "crumpled sheets stay in this browser");
        assert_eq!(sharing_target(Some("abcdefghijklmnop_-12")), Some("abcdefghijklmnop_-12"));
        for bad in [None, Some(""), Some("short"), Some("../../etc/passwd/xx"), Some("a b c d e f g h i j")] {
            assert_eq!(sharing_target(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn a_lone_surrogate_the_js_page_wrote_still_loads() {
        // JSON.stringify("\ud83d") is "\ud83d", which JSON.parse reads back
        let s = r#"{"v":1,"a":[{"id":"t","pts":[0,0,0.5],"txt":"\ud83d"}]}"#;
        let mut b = Board::default();
        let (st, mine) = Local::load(Some(s), &mut b);
        assert!(!st.damaged());
        assert_eq!(mine, ["t"]);
        assert_eq!(b.strokes[0].txt.as_deref(), Some("\u{fffd}"));
        // a lone low half, a pair split by text, an escaped backslash before a u, and a real pair
        let s = r#"{"v":1,"a":[{"id":"t","pts":[0],"txt":"a\udc00b\ud83dx\ude00\\ud83d\ud83d\ude00"}]}"#;
        let mut b = Board::default();
        let (st, _) = Local::load(Some(s), &mut b);
        assert!(!st.damaged());
        assert_eq!(b.strokes[0].txt.as_deref(), Some("a\u{fffd}b\u{fffd}x\u{fffd}\\ud83d\u{1f600}"));
        assert_eq!(mend_surrogates(JS_WROTE), JS_WROTE, "well-formed text is untouched");
    }

    #[test]
    fn the_shelf_keeps_the_last_five_and_next_k_follows_it() {
        let shelf: Vec<String> = (1..=7).map(|k| format!(r#"{{"k":{k},"a":[]}}"#)).collect();
        let s = format!(r#"{{"v":1,"a":[],"shelf":[{}],"k":2}}"#, shelf.join(","));
        let (st, _) = Local::load(Some(&s), &mut Board::default());
        assert_eq!(st.shelf.iter().map(|h| h.k).collect::<Vec<_>>(), [3, 4, 5, 6, 7]);
        assert_eq!(st.next_k, 8);
        assert_eq!(to_int32(Some(&serde_json::json!(4294967301.7))), 5);
        assert_eq!(to_int32(Some(&serde_json::json!(-2.5))), -2);
        assert_eq!(to_int32(Some(&serde_json::json!(2147483648.0))), -2147483648);
    }

    #[test]
    fn saving_writes_once_per_change_and_toasts_once_when_full() {
        let mut b = board(&["a"]);
        let mut st = Local::default();
        let Save::Write { sig, text } = st.save(&b, None) else { panic!() };
        assert_eq!(sig, "1:a:");
        assert_eq!(text, r##"{"v":1,"a":[{"id":"a","l":1,"c":"#111111","w":3,"pts":[0.1,0.2,0.5]}],"shelf":[],"k":1}"##);
        assert!(!st.saved(sig, &text, true));
        let stored = text;
        assert_eq!(st.save(&b, Some(&stored)), Save::Unchanged);
        b.insert(it("b"));
        let Save::Write { sig, text } = st.save(&b, Some(&stored)) else { panic!() };
        assert!(st.saved(sig.clone(), &text, false), "the first refusal toasts");
        assert!(!st.saved(sig.clone(), &text, false), "the second does not");
        assert!(matches!(st.save(&b, Some(&stored)), Save::Write { .. }), "a full store keeps trying");
        assert!(!st.saved(sig, &text, true));
    }

    #[test]
    fn a_second_tab_never_writes_over_the_first() {
        // Two tabs open the same private board.
        let stored = r##"{"v":1,"a":[{"id":"a","l":1,"c":"#111111","w":3,"pts":[0.1,0.2,0.5]}],"shelf":[],"k":1}"##;
        let (mut ba, mut bb) = (Board::default(), Board::default());
        let (mut ta, _) = Local::load(Some(stored), &mut ba);
        let (tb, _) = Local::load(Some(stored), &mut bb);
        assert!(!ta.elsewhere(Some(stored)) && !tb.elsewhere(Some(stored)));
        // Tab A draws and saves.
        ba.insert(it("from-a"));
        let Save::Write { sig, text } = ta.save(&ba, Some(stored)) else { panic!() };
        assert!(!ta.saved(sig, &text, true));
        let now = text;
        assert!(!ta.elsewhere(Some(&now)), "its own write is not someone else's");
        // Tab B draws: its save must not drop A's stroke; it is told to load again.
        bb.insert(it("from-b"));
        assert!(tb.elsewhere(Some(&now)), "a storage event tells B");
        assert_eq!(tb.save(&bb, Some(&now)), Save::Elsewhere);
        // Loaded again, B sees A's stroke and then saves over the new text.
        let mut bb = Board::default();
        let (tb2, _) = Local::load(Some(&now), &mut bb);
        assert!(bb.has("from-a"));
        bb.insert(it("from-b"));
        assert!(matches!(tb2.save(&bb, Some(&now)), Save::Write { .. }));
        // A private board removed (shared away) by another tab is a change too.
        assert!(tb2.elsewhere(None));
    }

    #[test]
    fn crumple_shelves_the_board_and_drops_the_sixth() {
        let mut st = Local::default();
        let mut b = board(&["a", "b"]);
        b.pending.insert("a".into(), it("a"));
        for k in 1..=5 {
            assert_eq!(
                st.crumple(&mut b),
                Some(In::Crumpled { k, by: Some(1), drop: None, shelf: st.shelf_list() })
            );
        }
        assert!(b.pending.is_empty());
        let Some(In::Crumpled { k: 6, drop: Some(1), shelf, .. }) = st.crumple(&mut b) else { panic!() };
        assert_eq!(shelf.iter().map(|h| (h.k, h.n)).collect::<Vec<_>>(), [(2, 2), (3, 2), (4, 2), (5, 2), (6, 2)]);
        assert_eq!(st.next_k, 7);
    }

    #[test]
    fn crumpling_a_blank_board_changes_nothing() {
        let mut st = Local::default();
        let mut b = board(&["a"]);
        st.crumple(&mut b).unwrap();
        let mut blank = board(&[]);
        blank.pending.insert("x".into(), it("x"));
        assert_eq!(st.crumple(&mut blank), None);
        assert_eq!((st.shelf.len(), st.next_k, blank.pending.len()), (1, 2, 1));
    }

    #[test]
    fn uncrumple_swaps_the_board_with_the_sheet() {
        let mut st = Local::default();
        let mut b = board(&["a"]);
        st.crumple(&mut b);
        b.drop_all();
        assert_eq!(st.uncrumple(&mut b, 9), None);
        // an empty board does not go on the shelf
        let (In::Uncrumpled { k: 1, a, shelf, .. }, repend) = st.uncrumple(&mut b, 1).unwrap() else { panic!() };
        assert_eq!((a.len(), shelf.len(), st.next_k), (1, 0, 2));
        for s in a {
            b.insert(s);
        }
        b.strokes[0].q = Some(4.0);
        repend.apply(&mut b);
        assert_eq!((b.strokes[0].q, b.pending.len()), (None, 1));
        // a board with items does, as sheet 2; the sheet view sees it
        st.crumple(&mut b); // sheet 2
        let (In::Uncrumpled { shelf, .. }, _) = st.uncrumple(&mut b, 2).unwrap() else { panic!() };
        assert_eq!(shelf, [ShelfEntry { k: 3, n: 1 }]);
        assert!(matches!(st.sheet_view(3), Some(In::SheetView { k: 3, a: Some(a) }) if a.len() == 1));
        assert_eq!(st.sheet_view(2), None);
    }

    #[test]
    fn the_shelf_holds_items_as_item_of_makes_them() {
        // a shared board's items (other layers, confirmed) made private keep only itemOf's fields
        let mut st = Local::default();
        let mut b = board(&["a"]);
        b.strokes[0].l = Some(7);
        b.strokes[0].q = Some(3.0);
        st.crumple(&mut b);
        assert_eq!(st.shelf[0].a[0], Item { l: Some(1), q: None, ..b.strokes[0].clone() });
        b.strokes[0].l = Some(7);
        b.strokes[0].q = Some(3.0);
        let k = st.next_k;
        st.crumple(&mut b);
        let (In::Uncrumpled { a, .. }, _) = st.uncrumple(&mut b, k).unwrap() else { panic!() };
        assert_eq!((a[0].l, a[0].q), (Some(1), None));
        assert_eq!(item_of(&Item { txt: Some("t".into()), ..b.strokes[0].clone() }).txt.as_deref(), Some("t"));
    }

    #[test]
    fn hidden_layers_round_trip_in_order() {
        let h = parse_hidden(Some("[3,1,2.0,2.5,\"4\",3,-1]"));
        assert_eq!(h.iter().copied().collect::<Vec<_>>(), [3, 1, 2, -1]);
        assert_eq!(hidden_json(&h), "[3,1,2,-1]");
        for bad in [None, Some(""), Some("{}"), Some("[")] {
            assert!(parse_hidden(bad).is_empty());
        }
    }

    #[test]
    fn secrets() {
        assert!(valid_secret("abcdefghijklmnop") && valid_secret(&"_-9Z".repeat(16)));
        for bad in ["", "short", "abcdefghijklmno", "abcdefghijklmnop=", "abcdefghijklmnop\n", &"a".repeat(65)] {
            assert!(!valid_secret(bad), "{bad:?}");
        }
        // node: Buffer.from(bytes).toString('base64url')
        let b: [u8; 24] = core::array::from_fn(|i| (i * 37 + 251) as u8);
        assert_eq!(secret_from(&b), "-yBFao-02f4jSG2St9wBJktwlbrfBClO");
        assert_eq!(secret_from(&[255; 24]), "_".repeat(32));
        assert!(valid_secret(&secret_from(&b)));
    }

    #[test]
    fn routes() {
        assert_eq!(route("/"), Route::Local);
        assert_eq!(route("/index.html"), Route::Local);
        assert_eq!(route("/b/Ab_-9"), Route::Board("Ab_-9".into()));
        assert_eq!(route("/b/Ab_-9/"), Route::Board("Ab_-9".into()));
        assert_eq!(route(&format!("/b/{}", "k".repeat(64))), Route::Board("k".repeat(64)));
        for p in ["/b/", "/b//", "/b/a//", "/b/a.b", "/b/a/c", "/x", "", "/index.htm", &format!("/b/{}", "k".repeat(65))] {
            assert_eq!(route(p), Route::Missing, "{p:?}");
        }
    }
}
