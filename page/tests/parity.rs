//! geom and view against app.js itself: random strokes and view actions run through the
//! JavaScript functions (lifted out of app.js by `parity/geom_view_ref.js`) and through the
//! Rust ports must give the same numbers, bit for bit.
//!
//! The reference is app.js as of the commit the port started from, read with `git show`, so
//! the test still runs once the switch-over deletes it. It needs `git` with that commit and
//! `node`; without either it says why and passes.

use serde_json::{json, Value};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use wb_page::{geom::*, view::*};

/// app.js as the port began.
const REF: &str = "f25aedd:web/app.js";

fn run(cmd: &mut Command, input: Option<&[u8]>) -> Option<Vec<u8>> {
    let mut c = cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::inherit()).spawn().ok()?;
    let mut stdin = c.stdin.take()?;
    if let Some(i) = input {
        stdin.write_all(i).ok()?;
    }
    drop(stdin);
    let o = c.wait_with_output().ok()?;
    o.status.success().then_some(o.stdout)
}

fn ops(o: &[Op]) -> Value {
    Value::Array(
        o.iter()
            .flat_map(|o| match o {
                Op::LineWidth(w) => vec![json!(["w", w])],
                Op::Begin => vec![json!(["b"])],
                Op::MoveTo(x, y) => vec![json!(["m", x, y])],
                Op::Quad(a, b, c, d) => vec![json!(["q", a, b, c, d])],
                Op::Bezier(a, b, c, d, e, f) => vec![json!(["c", a, b, c, d, e, f])],
                Op::Stroke => vec![json!(["s"])],
                // app.js's dot: beginPath, arc, fill (the fill is not recorded)
                Op::Disc(x, y, r) => vec![json!(["b"]), json!(["d", x, y, r])],
                // shapes and lines are Rust-only (A-166): app.js never drew them
                _ => unreachable!("not a stroke op"),
            })
            .collect(),
    )
}

fn rust(cases: &[Value]) -> Vec<Value> {
    let f = |v: &Value| v.as_f64().unwrap();
    cases
        .iter()
        .map(|c| {
            let pts: Vec<f64> = c["pts"].as_array().unwrap().iter().map(f).collect();
            let (px, w, bw) = (f(&c["px"]), f(&c["w"]), f(&c["bw"]));
            let mut v = View::default();
            v.layout(f(&c["vw"]), f(&c["vh"]), 1.0);
            let mut views = vec![];
            for a in c["acts"].as_array().unwrap() {
                let a = a.as_array().unwrap();
                let n = |i: usize| f(&a[i]);
                match a[0].as_str().unwrap() {
                    "za" => drop(v.zoom_at(n(1), n(2), n(3))),
                    "home" => drop(v.zoom_home()),
                    "ps" => drop(v.page_step(n(1), n(2))),
                    "set" => drop(v.set(n(1), n(2), n(3))),
                    "rs" => drop(v.layout(n(1), n(2), 1.0)),
                    k => panic!("action {k}"),
                }
                let ch = v.chrome();
                views.push(json!([v.zoom, v.pan_x, v.pan_y, ch.hide_l, ch.hide_r, ch.hide_u, ch.hide_d]));
            }
            json!({"ct": curve_through(&pts, px), "st": steady(&pts, px), "full": ops(&full(&pts, w, bw)),
                "hue": hue_hex(f(&c["hue"])), "views": views})
        })
        .collect()
}

/// Every number as an f64: JSON from JavaScript writes 2.0 as `2`, which serde_json reads as
/// an integer, not equal to the f64 2.0.
fn floats(v: Value) -> Value {
    match v {
        Value::Number(n) => json!(n.as_f64().unwrap()),
        Value::Array(a) => Value::Array(a.into_iter().map(floats).collect()),
        Value::Object(o) => Value::Object(o.into_iter().map(|(k, v)| (k, floats(v))).collect()),
        v => v,
    }
}

#[test]
fn geom_and_view_match_app_js() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let Some(app) = run(Command::new("git").arg("-C").arg(dir).args(["show", REF]), None) else {
        eprintln!("parity: skipped, `git show {REF}` failed (no git, or a shallow clone)");
        return;
    };
    let Some(cases) = run(Command::new("node").arg(dir.join("tests/parity/geom_view_gen.js")), None) else {
        eprintln!("parity: skipped, no node");
        return;
    };
    let tmp = std::env::temp_dir().join(format!("wb-parity-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();
    let app_js = tmp.join("app.js");
    std::fs::write(&app_js, app).unwrap();
    let js = run(Command::new("node").arg(dir.join("tests/parity/geom_view_ref.js")).arg(&app_js), Some(&cases))
        .expect("the reference run failed");
    std::fs::remove_dir_all(&tmp).ok();
    let cases: Vec<Value> = serde_json::from_slice(&cases).unwrap();
    let js: Vec<Value> = serde_json::from_slice(&js).unwrap();
    let (js, rs): (Vec<_>, Vec<_>) = (js.into_iter().map(floats).collect(), rust(&cases).into_iter().map(floats).collect());
    assert_eq!(js.len(), rs.len());
    for (i, (j, r)) in js.iter().zip(&rs).enumerate() {
        for k in ["ct", "st", "full", "hue", "views"] {
            if j[k] != r[k] {
                let at = match (&j[k], &r[k]) {
                    (Value::Array(x), Value::Array(y)) => (0..x.len().max(y.len())).find(|&n| x.get(n) != y.get(n)),
                    _ => None,
                };
                let item = |v: &Value| at.and_then(|n| v.get(n)).map_or_else(|| v.to_string(), Value::to_string);
                panic!("case {i}: {k} differs at {at:?}\n  case {}\n  js {}\n  rs {}", cases[i], item(&j[k]), item(&r[k]));
            }
        }
    }
}
