//! Board keys, contributor secrets and stroke ids.

use base64::{engine::general_purpose::URL_SAFE_NO_PAD as B64, Engine};
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

pub const KEY_LEN: usize = 24;

fn url_safe(s: &str) -> bool {
    s.bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// A fresh edit key: 18 random bytes, 24 characters.
pub fn random_key() -> String {
    let mut b = [0u8; 18];
    getrandom::fill(&mut b).expect("the OS random source failed");
    B64.encode(b)
}

/// The view key that belongs to an edit key. One-way.
pub fn view_key(edit: &str) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(edit.as_bytes()).expect("any key length works");
    mac.update(b"wb-view-v1");
    let mut s = B64.encode(mac.finalize().into_bytes());
    s.truncate(KEY_LEN);
    s
}

pub fn hash(s: &str) -> [u8; 32] {
    Sha256::digest(s.as_bytes()).into()
}

pub fn valid_key(s: &str) -> bool {
    s.len() == KEY_LEN && url_safe(s)
}

pub fn valid_who(s: &str) -> bool {
    (16..=64).contains(&s.len()) && url_safe(s)
}

pub fn valid_id(s: &str) -> bool {
    (1..=40).contains(&s.len()) && url_safe(s)
}

pub fn valid_color(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 7 && b[0] == b'#' && b[1..].iter().all(u8::is_ascii_hexdigit)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_well_formed_and_distinct() {
        let a = random_key();
        let b = random_key();
        assert!(valid_key(&a) && valid_key(&b));
        assert_ne!(a, b);
        let v = view_key(&a);
        assert!(valid_key(&v));
        assert_ne!(v, a);
        assert_eq!(v, view_key(&a));
        assert_ne!(v, view_key(&b));
    }

    #[test]
    fn validators() {
        assert!(valid_id("abc-DEF_123"));
        assert!(!valid_id(""));
        assert!(!valid_id("has space"));
        assert!(!valid_id(&"x".repeat(41)));
        assert!(valid_color("#a1B2c3"));
        assert!(!valid_color("#a1B2c"));
        assert!(!valid_color("a1B2c3f"));
        assert!(!valid_color("#a1B2cg"));
        assert!(valid_who(&"w".repeat(16)));
        assert!(!valid_who(&"w".repeat(15)));
        assert!(!valid_key("short"));
    }
}
