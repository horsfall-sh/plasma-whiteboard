//! JSON out, byte for byte as the browser's `JSON.stringify` writes it.
//!
//! The wire and the private-board store were written by `JSON.stringify` before this page was
//! Rust, and they must stay the same: a number prints as JavaScript prints it (`1`, not `1.0`;
//! `1e-7`, not `1e-07`), a missing field is left out rather than written as `null`, and keys
//! keep the order they were written in.

use std::fmt::Write;

/// A JSON value to write. Object keys keep their order.
#[derive(Clone, Debug, PartialEq)]
pub enum J {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<J>),
    Obj(Vec<(String, J)>),
}

impl J {
    /// An object from `(key, value)` pairs, in order.
    pub fn obj<'a>(fields: impl IntoIterator<Item = (&'a str, J)>) -> J {
        J::Obj(fields.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
    }

    pub fn str(s: impl Into<String>) -> J {
        J::Str(s.into())
    }

    /// A flat array of numbers, as a point list is sent.
    pub fn nums(v: &[f64]) -> J {
        J::Arr(v.iter().map(|&x| J::Num(x)).collect())
    }

    /// `JSON.stringify(this)`.
    pub fn stringify(&self) -> String {
        let mut out = String::new();
        self.write(&mut out);
        out
    }

    fn write(&self, out: &mut String) {
        match self {
            J::Null => out.push_str("null"),
            J::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            J::Num(v) => {
                if v.is_finite() {
                    push_num(out, *v)
                } else {
                    out.push_str("null") // as JSON.stringify writes NaN and Infinity
                }
            }
            J::Str(s) => push_str(out, s),
            J::Arr(a) => {
                out.push('[');
                for (i, v) in a.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    v.write(out);
                }
                out.push(']');
            }
            J::Obj(fields) => {
                out.push('{');
                for (i, (k, v)) in fields.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    push_str(out, k);
                    out.push(':');
                    v.write(out);
                }
                out.push('}');
            }
        }
    }
}

/// A string as `JSON.stringify` quotes it: `"` and `\` escaped, the C0 controls as their
/// short escape or `\u00xx` (lowercase hex), everything else as is.
pub fn push_str(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// A finite number as JavaScript's `Number.prototype.toString()` writes it (ECMA-262
/// Number::toString, radix 10): the shortest digits that read back to the same value, plain
/// for a decimal point position from -6 to 21, exponent form otherwise.
pub fn push_num(out: &mut String, v: f64) {
    if v == 0.0 {
        out.push('0'); // also -0, which JavaScript prints as 0
        return;
    }
    if v < 0.0 {
        out.push('-');
    }
    // Rust's `{:e}` finds how many digits the shortest round-trip form needs ("d.ddde-5" or
    // "de7"), but where two such digit strings are equally close it does not always pick the
    // even one, which ECMA-262 requires. Formatting again at that precision is exact and rounds
    // a tie to even. Just above a power of two the gap below `v` is half the gap above, so that
    // nearest string can fall outside the values that read back to `v`; then the shortest form
    // is the only string of that length that does, and it is used as it is.
    let short = format!("{:e}", v.abs());
    let k = short.split_once('e').map_or(1, |(m, _)| m.chars().filter(|c| *c != '.').count());
    let even = format!("{:.*e}", k - 1, v.abs());
    let e = if even.parse::<f64>().ok() == Some(v.abs()) { even } else { short };
    let (mant, exp) = e.split_once('e').expect("{:e} has an exponent");
    let exp: i32 = exp.parse().expect("an integer exponent");
    let digits: String = mant.chars().filter(|c| *c != '.').collect();
    let k = digits.len() as i32;
    let n = exp + 1; // the decimal point sits after `n` digits
    if k <= n && n <= 21 {
        out.push_str(&digits);
        for _ in 0..(n - k) {
            out.push('0');
        }
    } else if 0 < n && n <= 21 {
        out.push_str(&digits[..n as usize]);
        out.push('.');
        out.push_str(&digits[n as usize..]);
    } else if -6 < n && n <= 0 {
        out.push_str("0.");
        for _ in 0..(-n) {
            out.push('0');
        }
        out.push_str(&digits);
    } else {
        out.push_str(&digits[..1]);
        if k > 1 {
            out.push('.');
            out.push_str(&digits[1..]);
        }
        out.push('e');
        out.push(if n - 1 >= 0 { '+' } else { '-' });
        let _ = write!(out, "{}", (n - 1).abs());
    }
}

/// `String(v)` for a finite number.
pub fn num_to_string(v: f64) -> String {
    let mut s = String::new();
    if v.is_finite() {
        push_num(&mut s, v)
    } else if v.is_nan() {
        s.push_str("NaN")
    } else {
        s.push_str(if v > 0.0 { "Infinity" } else { "-Infinity" })
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(v: f64) -> String {
        num_to_string(v)
    }

    /// Each expected string is what a browser prints for `String(v)`.
    #[test]
    fn numbers_print_as_javascript_prints_them() {
        let cases: &[(f64, &str)] = &[
            (0.0, "0"),
            (-0.0, "0"),
            (1.0, "1"),
            (-1.0, "-1"),
            (0.004, "0.004"),
            (0.5625, "0.5625"),
            (0.1 + 0.2, "0.30000000000000004"),
            (123456.0, "123456"),
            (1e21, "1e+21"),
            (1e20, "100000000000000000000"),
            (123e18, "123000000000000000000"),
            (1.5e21, "1.5e+21"),
            (1e-6, "0.000001"),
            (1e-7, "1e-7"),
            (1.25e-7, "1.25e-7"),
            (0.00025, "0.00025"),
            (-0.12345, "-0.12345"),
            (2.0f32 as f64 / 3.0, "0.6666666666666666"),
            (0.1f32 as f64, "0.10000000149011612"),
            (5e-324, "5e-324"),
            // exact ties in the last digit go to the even digit, as V8 prints them
            (1.48880767822265625, "1.4888076782226562"),
            (-0.93308258056640625, "-0.9330825805664062"),
            (f64::from_bits(0x426979e605ecb100), "875354402661.5312"),
            (f64::from_bits(0xc3159b5a90a1d0a1), "-1520446946702376.2"),
            (f64::MAX, "1.7976931348623157e+308"),
        ];
        for &(v, want) in cases {
            assert_eq!(n(v), want, "{v:e}");
        }
        assert_eq!(n(f64::NAN), "NaN");
        assert_eq!(J::Num(f64::INFINITY).stringify(), "null");
    }

    #[test]
    fn strings_are_escaped_as_json_stringify_escapes_them() {
        let s = "a\"b\\c\nd\te\r\u{8}\u{c}\u{1}\u{1f}\u{7f}é\u{2028}😀";
        assert_eq!(
            J::str(s).stringify(),
            "\"a\\\"b\\\\c\\nd\\te\\r\\b\\f\\u0001\\u001f\u{7f}é\u{2028}😀\""
        );
    }

    #[test]
    fn objects_keep_their_key_order() {
        let m = J::obj([
            ("t", J::str("s")),
            ("id", J::str("x1")),
            ("w", J::Num(0.004)),
            ("pts", J::nums(&[0.0, 0.5, 1.0])),
            ("ok", J::Bool(true)),
            ("who", J::Null),
        ]);
        assert_eq!(
            m.stringify(),
            r#"{"t":"s","id":"x1","w":0.004,"pts":[0,0.5,1],"ok":true,"who":null}"#
        );
    }
    #[test]
    fn powers_of_two_read_back_exactly() {
        // where the gap below a value is half the gap above, the nearest string of the shortest
        // length can fall outside the lower half-gap; every power of two must still read back
        let mut v = f64::from_bits(1); // 2^-1074
        let mut seen = 0;
        while v.is_finite() {
            for x in [v, -v, v * 3.0, v * 1.5] {
                if !x.is_finite() {
                    continue;
                }
                let mut s = String::new();
                push_num(&mut s, x);
                assert_eq!(s.parse::<f64>().ok(), Some(x), "{s}");
            }
            v *= 2.0;
            seen += 1;
        }
        assert_eq!(seen, 2098);
    }
}
