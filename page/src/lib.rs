//! The Plasma Whiteboard page. The pure modules (`json`, `proto`, `model`, `view`, `geom`,
//! `local`) know nothing of the browser and are tested on the host with `cargo test`; the web
//! modules drive the DOM, the canvases and the socket through web-sys.

pub mod json;
pub mod proto;
pub mod model;
pub mod view;
pub mod geom;
pub mod local;
