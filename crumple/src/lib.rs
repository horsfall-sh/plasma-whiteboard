//! The crumple animation: a sheet of paper that wrinkles, balls up and is
//! tossed aside. Built for wasm32-unknown-unknown with no dependencies; the
//! page calls the raw exports and draws the mesh with WebGL, using a picture
//! of the board as the paper's texture.
//!
//! The sheet is a grid of (cols + 1) x (rows + 1) vertices. For a progress
//! value t in 0..=1, `cr_frame` writes four f32 per vertex: x, y (board-width
//! units: x 0..1, y 0..aspect), depth (-1 far .. 1 near) and a shade factor.
//! t = 0 is the flat sheet exactly; t = 1 is a small wad at the target point.
//! Playing t backwards uncrumples.

use core::f32::consts::PI;

pub struct Sheet {
    cols: usize,
    rows: usize,
    aspect: f32,
    seed: u32,
    target: (f32, f32),
    out: Vec<f32>,
    pos: Vec<[f32; 3]>,
}

fn hash(x: i32, y: i32, seed: u32) -> f32 {
    let mut h = (x as u32).wrapping_mul(0x9E37_79B1) ^ (y as u32).wrapping_mul(0x85EB_CA77) ^ seed.wrapping_mul(0xC2B2_AE3D);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2C1B_3C6D);
    h ^= h >> 12;
    h = h.wrapping_mul(0x297A_2D39);
    h ^= h >> 15;
    (h as f32 / u32::MAX as f32) * 2.0 - 1.0
}

/// Smooth value noise in -1..1.
fn noise(x: f32, y: f32, seed: u32) -> f32 {
    let (xi, yi) = (x.floor(), y.floor());
    let (fx, fy) = (x - xi, y - yi);
    let (sx, sy) = (fx * fx * (3.0 - 2.0 * fx), fy * fy * (3.0 - 2.0 * fy));
    let (xi, yi) = (xi as i32, yi as i32);
    let a = hash(xi, yi, seed);
    let b = hash(xi + 1, yi, seed);
    let c = hash(xi, yi + 1, seed);
    let d = hash(xi + 1, yi + 1, seed);
    a + (b - a) * sx + (c - a) * sy + (a - b - c + d) * sx * sy
}

/// Two octaves.
fn fbm(x: f32, y: f32, seed: u32) -> f32 {
    noise(x, y, seed) * 0.67 + noise(x * 2.3 + 7.1, y * 2.3 - 3.7, seed ^ 0x51ED) * 0.33
}

fn ease(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

impl Sheet {
    pub fn new(cols: usize, rows: usize, seed: u32, aspect: f32) -> Sheet {
        let cols = cols.clamp(2, 128);
        let rows = rows.clamp(2, 128);
        let n = (cols + 1) * (rows + 1);
        Sheet {
            cols,
            rows,
            aspect,
            seed,
            target: (0.5, aspect * 0.5),
            out: vec![0.0; n * 4],
            pos: vec![[0.0; 3]; n],
        }
    }

    pub fn set_target(&mut self, x: f32, y: f32) {
        self.target = (x, y);
    }

    pub fn vertices(&self) -> usize {
        (self.cols + 1) * (self.rows + 1)
    }

    pub fn data(&self) -> &[f32] {
        &self.out
    }

    /// Where vertex (u, v) of the sheet is at progress t.
    fn place(&self, u: f32, v: f32, t: f32) -> [f32; 3] {
        let a = self.aspect;
        let s = self.seed;
        let flat = [u, v * a, 0.0];
        if t <= 0.0 {
            return flat;
        }
        // 0..0.72: wrinkle and gather into a wad. 0.62..1: toss the wad away.
        let gather = ease(t / 0.72);
        let toss = ease((t - 0.62) / 0.38);
        let centre = [0.5, a * 0.5];

        // Each point of the sheet has a place on a lumpy ball. Low-frequency
        // noise keeps neighbours together, so the paper folds, not shatters.
        let f = 2.6;
        let mut d = [fbm(u * f, v * f, s), fbm(u * f + 31.4, v * f + 4.2, s ^ 0xA5A5), fbm(u * f - 12.9, v * f + 47.3, s ^ 0x3C3C)];
        // Pull the direction outward from the sheet's centre a little, so the
        // wad keeps a trace of the sheet's layout.
        d[0] += (u - 0.5) * 0.9;
        d[1] += (v - 0.5) * 0.9;
        let len = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt().max(1e-4);
        let lump = 0.62 + 0.38 * fbm(u * 5.0 + 3.3, v * 5.0 - 8.8, s ^ 0x7777).abs();
        let r = 0.105 * lump;
        let ball = [centre[0] + d[0] / len * r, centre[1] + d[1] / len * r, d[2] / len * r];

        // Sharp creases that rise early, before the sheet has moved much.
        let crease = (1.0 - fbm(u * 6.0 + 1.7, v * 6.0 * a + 9.1, s ^ 0x1234).abs() * 2.0) * 0.045;
        let wrinkle = (PI * (gather * 1.15).min(1.0)).sin();

        let g = gather * gather * (3.0 - 2.0 * gather);
        let mut p = [
            flat[0] + (ball[0] - flat[0]) * g,
            flat[1] + (ball[1] - flat[1]) * g,
            ball[2] * g + crease * wrinkle,
        ];

        // Turn the wad as it forms and flies, shrink it, and move it along an
        // arc to the target.
        let angle = gather * 0.5 + toss * 2.6;
        let (sn, cs) = angle.sin_cos();
        let scale = 1.0 - 0.55 * toss;
        let (dx, dy) = (p[0] - centre[0], p[1] - centre[1]);
        let spin = g; // no rotation while still flat
        let (rx, ry) = (dx * cs - dy * sn, dx * sn + dy * cs);
        let (dx, dy) = (dx + (rx - dx) * spin, dy + (ry - dy) * spin);
        p[0] = centre[0] + dx * scale + (self.target.0 - centre[0]) * toss;
        p[1] = centre[1] + dy * scale + (self.target.1 - centre[1]) * toss - 0.12 * (PI * toss).sin();
        p[2] *= scale;
        p
    }

    /// Compute every vertex for progress t.
    pub fn frame(&mut self, t: f32) {
        let t = if t.is_finite() { t.clamp(0.0, 1.0) } else { 0.0 };
        let (c, r) = (self.cols, self.rows);
        for j in 0..=r {
            for i in 0..=c {
                self.pos[j * (c + 1) + i] = self.place(i as f32 / c as f32, j as f32 / r as f32, t);
            }
        }
        // Shade from the surface normal, so folds read as light and shadow.
        let lit = ease(t * 5.0);
        let light = [-0.35f32, -0.5, 0.79];
        for j in 0..=r {
            for i in 0..=c {
                let at = |i: usize, j: usize| self.pos[j * (c + 1) + i];
                let (l, rt) = (at(i.saturating_sub(1), j), at((i + 1).min(c), j));
                let (up, dn) = (at(i, j.saturating_sub(1)), at(i, (j + 1).min(r)));
                let du = [rt[0] - l[0], rt[1] - l[1], rt[2] - l[2]];
                let dv = [dn[0] - up[0], dn[1] - up[1], dn[2] - up[2]];
                let n = [du[1] * dv[2] - du[2] * dv[1], du[2] * dv[0] - du[0] * dv[2], du[0] * dv[1] - du[1] * dv[0]];
                let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
                let facing = if len > 1e-9 { ((n[0] * light[0] + n[1] * light[1] + n[2] * light[2]) / len).abs() } else { 0.79 };
                let shade = (0.5 + 0.64 * facing).min(1.06);
                let p = at(i, j);
                let o = (j * (c + 1) + i) * 4;
                self.out[o] = p[0];
                self.out[o + 1] = p[1];
                self.out[o + 2] = (p[2] * 4.0).clamp(-1.0, 1.0);
                self.out[o + 3] = 1.0 + (shade - 1.0) * lit;
            }
        }
    }
}

// ---- raw exports for the page -------------------------------------------------

static mut SHEET: Option<Sheet> = None;

#[allow(static_mut_refs)]
fn sheet() -> Option<&'static mut Sheet> {
    // The page is single-threaded and never re-enters these calls.
    unsafe { SHEET.as_mut() }
}

/// Make a sheet of cols x rows cells. Returns the number of vertices.
#[no_mangle]
pub extern "C" fn cr_setup(cols: u32, rows: u32, seed: u32, aspect: f32) -> u32 {
    let s = Sheet::new(cols as usize, rows as usize, seed, aspect);
    let n = s.vertices() as u32;
    #[allow(static_mut_refs)]
    unsafe {
        SHEET = Some(s);
    }
    n
}

/// Where the wad ends up, in board units.
#[no_mangle]
pub extern "C" fn cr_target(x: f32, y: f32) {
    if let Some(s) = sheet() {
        s.set_target(x, y);
    }
}

/// Compute the mesh for progress t (0 flat .. 1 gone). Returns a pointer to
/// vertices x 4 f32 (x, y, depth, shade), valid until the next call.
#[no_mangle]
pub extern "C" fn cr_frame(t: f32) -> *const f32 {
    match sheet() {
        Some(s) => {
            s.frame(t);
            s.out.as_ptr()
        }
        None => core::ptr::null(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const A: f32 = 0.5625;

    #[test]
    fn starts_as_the_flat_sheet() {
        let mut s = Sheet::new(8, 4, 7, A);
        s.frame(0.0);
        let d = s.data();
        for j in 0..=4 {
            for i in 0..=8 {
                let o = (j * 9 + i) * 4;
                assert!((d[o] - i as f32 / 8.0).abs() < 1e-6);
                assert!((d[o + 1] - j as f32 / 4.0 * A).abs() < 1e-6);
                assert_eq!(d[o + 2], 0.0);
                assert!((d[o + 3] - 1.0).abs() < 1e-6, "no shading on the flat sheet");
            }
        }
    }

    #[test]
    fn ends_as_a_small_wad_at_the_target() {
        let mut s = Sheet::new(40, 24, 99, A);
        s.set_target(0.9, 0.1);
        s.frame(1.0);
        for v in s.data().chunks(4) {
            let dist = ((v[0] - 0.9).powi(2) + (v[1] - 0.1).powi(2)).sqrt();
            assert!(dist < 0.08, "vertex {dist} from the target");
        }
    }

    #[test]
    fn every_frame_is_finite_and_in_range() {
        let mut s = Sheet::new(40, 24, 3, A);
        for k in 0..=100 {
            s.frame(k as f32 / 100.0);
            for v in s.data().chunks(4) {
                assert!(v.iter().all(|x| x.is_finite()));
                assert!((-0.5..1.5).contains(&v[0]) && (-0.5..1.5).contains(&v[1]));
                assert!((-1.0..=1.0).contains(&v[2]));
                assert!((0.4..=1.1).contains(&v[3]));
            }
        }
        s.frame(f32::NAN);
        assert!(s.data().iter().all(|x| x.is_finite()));
    }

    #[test]
    fn the_sheet_shrinks_as_it_gathers() {
        let spread = |s: &Sheet| {
            let (mut lo, mut hi) = (f32::MAX, f32::MIN);
            for v in s.data().chunks(4) {
                lo = lo.min(v[0]);
                hi = hi.max(v[0]);
            }
            hi - lo
        };
        let mut s = Sheet::new(40, 24, 5, A);
        let mut last = f32::MAX;
        for t in [0.0, 0.2, 0.4, 0.6, 0.72] {
            s.frame(t);
            let w = spread(&s);
            assert!(w <= last + 1e-4, "width grew at t={t}");
            last = w;
        }
        assert!(last < 0.3);
    }

    #[test]
    fn same_seed_same_wad_and_seeds_differ() {
        let run = |seed| {
            let mut s = Sheet::new(16, 9, seed, A);
            s.frame(0.5);
            s.data().to_vec()
        };
        assert_eq!(run(1), run(1));
        assert_ne!(run(1), run(2));
    }

    #[test]
    fn exports_round_trip() {
        assert_eq!(cr_setup(4, 2, 1, A), 15);
        cr_target(0.2, 0.2);
        let p = cr_frame(0.3);
        assert!(!p.is_null());
        let d = unsafe { core::slice::from_raw_parts(p, 15 * 4) };
        assert!(d.iter().all(|x| x.is_finite()));
    }
}
