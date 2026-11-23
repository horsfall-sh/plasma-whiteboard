//! The Plasma Whiteboard page, ported from `web/app.js`. Every module here is tested on the
//! host by `cargo test` (and so by `make test`): `json`, `proto`, `model`, `view`, `geom`,
//! `local`, `input` and `net` know nothing of the browser, and `render` draws through a
//! [`render::Surface`] trait that the browser's 2D context implements and the tests record.
//! `tests/parity.rs` checks `geom` and `view` against app.js itself. `names` holds the people
//! labels' pure helpers. `web` (wasm only) is the glue that wires these to the DOM, the socket
//! and storage; `fx` is the crumple animation's WebGL.

pub mod json;
pub mod proto;
pub mod model;
pub mod view;
pub mod geom;
pub mod local;
pub mod render;
pub mod input;
pub mod net;
pub mod names;
pub mod shape;
pub mod ask;
#[cfg(target_arch = "wasm32")]
pub mod fx;
#[cfg(target_arch = "wasm32")]
pub mod web;
