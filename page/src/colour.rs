//! The colour picker (A-166 item 15): eight fixed swatches, a shade area and hue strip seeded
//! from the current colour, and a few recent picks. All of it is this viewer's; only the
//! chosen `#rrggbb` goes on the wire, as every colour always has.

use crate::geom::ERASER_COLOR;

/// The fixed swatches, in toolbar order, with their names.
pub const SWATCHES: [(&str, &str); 8] = [
    ("#111111", "Black"),
    ("#868e96", "Grey"),
    ("#d63031", "Red"),
    ("#e67e22", "Orange"),
    ("#1e9e4a", "Green"),
    ("#0a66d8", "Blue"),
    ("#8e44ad", "Purple"),
    ("#fdfdfd", "White"),
];

/// How many recent picks the toolbar keeps.
pub const RECENT: usize = 3;

/// A swatch's name, if it is one.
pub fn name(c: &str) -> Option<&'static str> {
    SWATCHES.iter().find(|(h, _)| h.eq_ignore_ascii_case(c)).map(|(_, n)| *n)
}

/// `#rrggbb` as 0..=255 channels.
pub fn rgb(c: &str) -> Option<(u8, u8, u8)> {
    if !crate::shape::is_colour(c) {
        return None;
    }
    let p = |i: usize| u8::from_str_radix(&c[i..i + 2], 16).ok();
    Some((p(1)?, p(3)?, p(5)?))
}

/// Hue in degrees [0, 360), saturation and value in [0, 1].
pub fn to_hsv(c: &str) -> Option<(f64, f64, f64)> {
    let (r, g, b) = rgb(c)?;
    let [r, g, b] = [r, g, b].map(|v| v as f64 / 255.0);
    let max = r.max(g).max(b);
    let d = max - r.min(g).min(b);
    let h = if d == 0.0 {
        0.0
    } else if max == r {
        60.0 * ((g - b) / d).rem_euclid(6.0)
    } else if max == g {
        60.0 * ((b - r) / d + 2.0)
    } else {
        60.0 * ((r - g) / d + 4.0)
    };
    Some((h, if max == 0.0 { 0.0 } else { d / max }, max))
}

/// The colour at hue `h` (degrees), saturation `s`, value `v`, as lower-case `#rrggbb`. Pure
/// white comes out a hair off, since the eraser paints `#ffffff` and strokes in it are not
/// treated as mine to select or move.
pub fn from_hsv(h: f64, s: f64, v: f64) -> String {
    let (h, s, v) = (h.rem_euclid(360.0), s.clamp(0.0, 1.0), v.clamp(0.0, 1.0));
    let c = v * s;
    let x = c * (1.0 - ((h / 60.0).rem_euclid(2.0) - 1.0).abs());
    let (r, g, b) = match (h / 60.0) as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = v - c;
    let q = |t: f64| ((t + m) * 255.0).round().clamp(0.0, 255.0) as u8;
    let out = format!("#{:02x}{:02x}{:02x}", q(r), q(g), q(b));
    if out == ERASER_COLOR {
        SWATCHES[7].0.to_string()
    } else {
        out
    }
}

/// The picker's state: the hue strip and the shade area. Opening it seeds them from the
/// current colour; a grey keeps the hue the strip was last at, so it stays anchored.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Picker {
    pub h: f64,
    pub s: f64,
    pub v: f64,
}

impl Default for Picker {
    fn default() -> Self {
        Picker { h: 0.0, s: 1.0, v: 1.0 }
    }
}

impl Picker {
    pub fn seed(&mut self, c: &str) {
        if let Some((h, s, v)) = to_hsv(c) {
            if s > 0.0 && v > 0.0 {
                self.h = h;
            }
            self.s = s;
            self.v = v;
        }
    }

    /// A point on the shade area, as fractions of its width and height (top is bright).
    pub fn shade_at(&mut self, fx: f64, fy: f64) -> String {
        self.s = fx.clamp(0.0, 1.0);
        self.v = 1.0 - fy.clamp(0.0, 1.0);
        self.hex()
    }

    /// A point on the hue strip, as a fraction of its width.
    pub fn hue_at(&mut self, fx: f64) -> String {
        self.h = fx.clamp(0.0, 1.0) * 359.999;
        self.hex()
    }

    pub fn hex(&self) -> String {
        from_hsv(self.h, self.s, self.v)
    }
}

/// Put `c` first in `recent`: a swatch or my own colour is not recorded, a repeat moves up,
/// and only [`RECENT`] are kept.
pub fn remember(recent: &mut Vec<String>, c: &str, mine: Option<&str>) {
    if name(c).is_some() || mine.is_some_and(|m| m.eq_ignore_ascii_case(c)) || rgb(c).is_none() {
        return;
    }
    let c = c.to_ascii_lowercase();
    recent.retain(|r| *r != c);
    recent.insert(0, c);
    recent.truncate(RECENT);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hsv_round_trips_every_swatch() {
        for (c, _) in SWATCHES {
            let (h, s, v) = to_hsv(c).unwrap();
            assert_eq!(from_hsv(h, s, v), c);
        }
        assert_eq!(from_hsv(0.0, 1.0, 1.0), "#ff0000");
        assert_eq!(from_hsv(120.0, 1.0, 0.5), "#008000");
        assert_eq!(from_hsv(240.0, 1.0, 1.0), "#0000ff");
        assert_eq!(to_hsv("#zzzzzz"), None);
    }

    #[test]
    fn pure_white_is_never_the_eraser() {
        assert_eq!(from_hsv(10.0, 0.0, 1.0), "#fdfdfd");
        let mut p = Picker::default();
        assert_eq!(p.shade_at(0.0, 0.0), "#fdfdfd");
        assert!(SWATCHES.iter().all(|(c, _)| *c != ERASER_COLOR));
    }

    #[test]
    fn the_picker_is_seeded_from_the_swatch_and_keeps_its_hue_on_grey() {
        let mut p = Picker::default();
        p.seed("#0a66d8");
        assert!((p.h - 213.6).abs() < 0.5, "{}", p.h);
        assert_eq!(p.hex(), "#0a66d8");
        // a grey has no hue: the strip stays where blue left it
        p.seed("#868e96");
        assert!((p.h - 213.6).abs() < 0.5 || (p.h - 210.0).abs() < 0.5, "{}", p.h);
        let h = p.h;
        p.seed("#111111");
        assert_eq!(p.h, h);
        // the shade area's top right is the strip's pure hue; its bottom is black
        p.hue_at(0.0);
        assert_eq!(p.shade_at(1.0, 0.0), "#ff0000");
        assert_eq!(p.shade_at(0.3, 1.0), "#000000");
        assert_eq!(p.shade_at(2.0, -1.0), "#ff0000");
    }

    #[test]
    fn recent_keeps_three_custom_picks_newest_first() {
        let mut r = vec![];
        for c in ["#123456", "#D63031", "#abcdef", "#123456", "#000001", "#000002", "#5e5e5e"] {
            remember(&mut r, c, Some("#5e5e5e"));
        }
        assert_eq!(r, ["#000002", "#000001", "#123456"]);
        remember(&mut r, "nope", None);
        assert_eq!(r.len(), 3);
    }
}
