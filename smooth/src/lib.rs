//! Stroke simplification for the whiteboard, built for wasm32-unknown-unknown.
//! No dependencies and no wasm-bindgen: the page calls the raw exports below.
//!
//! A stroke is a flat f32 array of (x, y, pressure) triples. x and y are in
//! board-width units, so distances are isotropic.

/// Pressure difference that counts as much as `eps` of positional error.
const PRESSURE_SPAN: f32 = 0.15;

/// Ramer-Douglas-Peucker over (x, y, pressure) triples, compacting in place.
/// Returns the number of points kept. Iterative, so long strokes can't
/// overflow the stack.
pub fn simplify(pts: &mut [f32], eps: f32) -> usize {
    let n = pts.len() / 3;
    if n <= 2 {
        return n;
    }
    let e2 = eps * eps;
    let pscale = eps / PRESSURE_SPAN;
    let mut keep = vec![false; n];
    keep[0] = true;
    keep[n - 1] = true;
    let mut stack = vec![(0usize, n - 1)];

    while let Some((a, b)) = stack.pop() {
        if b <= a + 1 {
            continue;
        }
        let (ax, ay, ap) = (pts[a * 3], pts[a * 3 + 1], pts[a * 3 + 2]);
        let (dx, dy, dp) = (pts[b * 3] - ax, pts[b * 3 + 1] - ay, pts[b * 3 + 2] - ap);
        let len2 = dx * dx + dy * dy;
        let mut best = 0f32;
        let mut idx = a;
        for i in a + 1..b {
            let (px, py) = (pts[i * 3] - ax, pts[i * 3 + 1] - ay);
            let t = if len2 <= f32::EPSILON {
                0.0
            } else {
                ((px * dx + py * dy) / len2).clamp(0.0, 1.0)
            };
            let (qx, qy) = (px - t * dx, py - t * dy);
            let pe = (pts[i * 3 + 2] - (ap + t * dp)) * pscale;
            let d2 = qx * qx + qy * qy + pe * pe;
            if d2 > best {
                best = d2;
                idx = i;
            }
        }
        if best > e2 {
            keep[idx] = true;
            stack.push((a, idx));
            stack.push((idx, b));
        }
    }

    let mut out = 0;
    for (i, &kept) in keep.iter().enumerate() {
        if kept {
            if out != i {
                pts.copy_within(i * 3..i * 3 + 3, out * 3);
            }
            out += 1;
        }
    }
    out
}

/// Allocate room for `len` f32 values inside wasm memory.
#[no_mangle]
pub extern "C" fn wb_alloc(len: usize) -> *mut f32 {
    let mut v = Vec::<f32>::with_capacity(len);
    let p = v.as_mut_ptr();
    core::mem::forget(v);
    p
}

/// Free a buffer returned by `wb_alloc` (same `len`).
///
/// # Safety
/// `ptr` must come from `wb_alloc(len)` and not have been freed already.
#[no_mangle]
pub unsafe extern "C" fn wb_free(ptr: *mut f32, len: usize) {
    drop(Vec::from_raw_parts(ptr, 0, len));
}

/// Simplify `n` points at `ptr` in place; returns the new point count.
///
/// # Safety
/// `ptr` must point to at least `n * 3` initialised f32 values.
#[no_mangle]
pub unsafe extern "C" fn wb_simplify(ptr: *mut f32, n: usize, eps: f32) -> usize {
    simplify(core::slice::from_raw_parts_mut(ptr, n * 3), eps)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn straight_line_collapses_to_endpoints() {
        let mut p: Vec<f32> = (0..100).flat_map(|i| [i as f32 * 0.01, 0.2, 0.5]).collect();
        let n = simplify(&mut p, 0.001);
        assert_eq!(n, 2);
        assert_eq!(p[0], 0.0);
        assert!((p[3] - 0.99).abs() < 1e-6);
    }

    #[test]
    fn corner_is_kept() {
        let mut p = vec![0.0, 0.0, 0.5, 0.5, 0.0, 0.5, 0.5, 0.5, 0.5];
        assert_eq!(simplify(&mut p, 0.001), 3);
    }

    #[test]
    fn pressure_change_is_kept() {
        // Straight in x/y, but pressure spikes in the middle.
        let mut p = vec![0.0, 0.0, 0.2, 0.5, 0.0, 0.9, 1.0, 0.0, 0.2];
        assert_eq!(simplify(&mut p, 0.001), 3);
    }

    #[test]
    fn tiny_and_degenerate_inputs() {
        assert_eq!(simplify(&mut [], 0.001), 0);
        assert_eq!(simplify(&mut [0.1, 0.1, 0.5], 0.001), 1);
        let mut same = vec![0.3, 0.3, 0.5].repeat(50);
        assert_eq!(simplify(&mut same, 0.001), 2);
    }

    #[test]
    fn circle_stays_within_tolerance() {
        let eps = 0.002;
        let raw: Vec<f32> = (0..2000)
            .flat_map(|i| {
                let a = i as f32 / 2000.0 * core::f32::consts::TAU;
                [0.5 + 0.2 * a.cos(), 0.3 + 0.2 * a.sin(), 0.5]
            })
            .collect();
        let mut p = raw.clone();
        let n = simplify(&mut p, eps);
        assert!(n > 8 && n < 200, "kept {n}");
        // Every kept point is still on the circle.
        for c in p[..n * 3].chunks(3) {
            let r = ((c[0] - 0.5).powi(2) + (c[1] - 0.3).powi(2)).sqrt();
            assert!((r - 0.2).abs() < 1e-4);
        }
    }

    #[test]
    fn ffi_round_trip() {
        unsafe {
            let n = 300;
            let ptr = wb_alloc(n * 3);
            for i in 0..n {
                *ptr.add(i * 3) = i as f32 / n as f32;
                *ptr.add(i * 3 + 1) = 0.1;
                *ptr.add(i * 3 + 2) = 0.5;
            }
            assert_eq!(wb_simplify(ptr, n, 0.001), 2);
            wb_free(ptr, n * 3);
        }
    }
}
