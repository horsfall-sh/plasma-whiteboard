//! The Plasma Whiteboard page, ported from `web/app.js`. Every module here is tested on the
//! host by `cargo test` (and so by `make test`): `json`, `proto`, `model`, `view`, `geom`,
//! `local`, `input` and `net` know nothing of the browser, and `render` draws through a
//! [`render::Surface`] trait that the browser's 2D context implements and the tests record.
//! `tests/parity.rs` checks `geom` and `view` against app.js itself. The glue that wires these
//! to the DOM, the socket and storage is not written yet, so `web/index.html` still runs app.js.

pub mod json;
pub mod proto;
pub mod model;
pub mod view;
pub mod geom;
pub mod local;
pub mod render;
pub mod input;
pub mod net;
