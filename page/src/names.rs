//! People's labels, from app.js's people section: the random names (`rollName`) and the
//! cleaning of a typed one (`cleanName`). Pure, so they are tested on the host.

use crate::input::{js_space, js_trim};

pub const NAME_A: [&str; 32] = [
    "Amber", "Brisk", "Calm", "Clever", "Copper", "Dapper", "Eager", "Fuzzy", "Gentle", "Golden", "Happy", "Indigo",
    "Jolly", "Keen", "Lively", "Lucky", "Mellow", "Nimble", "Olive", "Plucky", "Quiet", "Rapid", "Rusty", "Silver",
    "Sunny", "Tidy", "Velvet", "Witty", "Zesty", "Bold", "Cosy", "Merry",
];

pub const NAME_B: [&str; 32] = [
    "Otter", "Heron", "Badger", "Falcon", "Lynx", "Marten", "Newt", "Osprey", "Panda", "Quail", "Raven", "Seal",
    "Tapir", "Vole", "Walrus", "Yak", "Zebra", "Beaver", "Crane", "Dingo", "Egret", "Finch", "Gecko", "Hare", "Ibis",
    "Jackal", "Koala", "Lemur", "Moose", "Robin", "Stoat", "Wren",
];

/// The longest name, in UTF-16 units as the page's `maxlength` and `slice(0, 24)` count.
pub const NAME_MAX: usize = 24;

/// `pick(a)` for `Math.random()` = `r` in [0, 1).
fn pick(a: &[&'static str], r: f64) -> &'static str {
    let i = (r * a.len() as f64).floor();
    let i = if i.is_finite() && i >= 0.0 { (i as usize).min(a.len() - 1) } else { 0 };
    a[i]
}

/// `rollName()`, given two draws of `Math.random()`.
pub fn roll_name(r1: f64, r2: f64) -> String {
    format!("{} {}", pick(&NAME_A, r1), pick(&NAME_B, r2))
}

/// `cleanName(t)`: no control characters, trimmed, inner runs of white space made one space,
/// and at most [`NAME_MAX`] UTF-16 units. Unlike JavaScript's `slice`, a surrogate pair at the
/// cut is dropped whole rather than split in half.
pub fn clean_name(t: &str) -> String {
    let kept: String = t.chars().filter(|&c| !(c <= '\u{1f}' || c == '\u{7f}')).collect();
    let words: Vec<&str> = js_trim(&kept).split(js_space).filter(|w| !w.is_empty()).collect();
    let joined = words.join(" ");
    let mut out = String::new();
    let mut units = 0;
    for c in joined.chars() {
        let n = c.len_utf16();
        if units + n > NAME_MAX {
            break;
        }
        units += n;
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rolls_from_both_lists() {
        assert_eq!(roll_name(0.0, 0.0), "Amber Otter");
        assert_eq!(roll_name(0.999_999, 0.999_999), "Merry Wren");
        assert_eq!(roll_name(0.5, 0.25), "Mellow Panda");
        // out of range never panics
        assert_eq!(roll_name(1.0, -1.0), "Merry Otter");
        assert_eq!(roll_name(f64::NAN, 0.0), "Amber Otter");
    }

    #[test]
    fn cleans_a_typed_name() {
        assert_eq!(clean_name("  Ada \t\n  Lovelace  "), "Ada Lovelace");
        assert_eq!(clean_name("A\u{0}d\u{7f}a\u{1b}"), "Ada");
        assert_eq!(clean_name("   "), "");
        assert_eq!(clean_name("\u{feff}Ada\u{a0}B\u{2003}C"), "Ada B C");
        assert_eq!(clean_name(&"x".repeat(30)), "x".repeat(24));
    }

    #[test]
    fn cuts_at_24_units_without_splitting_a_pair() {
        let s = format!("{}\u{1F600}", "a".repeat(23));
        assert_eq!(clean_name(&s), "a".repeat(23));
        let s = format!("{}\u{1F600}b", "a".repeat(22));
        assert_eq!(clean_name(&s), format!("{}\u{1F600}", "a".repeat(22)));
    }
}
