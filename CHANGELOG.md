# Changelog

## v0.2.0 (unreleased)

The page is now Rust, built to WebAssembly (`page/`). `web/app.js` is gone: `web/boot.js`
loads `page.js` and `page_bg.wasm`, and a browser without WebAssembly is told so. The wire
protocol is unchanged, so the relay serves old and new pages alike.

### The port

- The board model, the wire protocol, the view (zoom and pan), the shapes, the private board,
  the canvases and the time-sliced redraw, pointer input and text boxes, the relay connection,
  the crumple and unfold effect in WebGL, and the browser glue. Each part is tested on the host
  by `make test`; the geometry and the view are checked against the JavaScript page itself.
- `make wasm` checks that wasm-bindgen-cli is 0.2.100, the version the page pins.

### Fixes from the review council

- F1: a private board that cannot be read is kept, said so at once, offered as a download, and
  replaced only on "Start over".
- F2: the geometry functions are total on short point lists.
- F3: the relay fails closed when a board cannot be loaded.
- F4: a stroke is acknowledged only once it is committed.
- F5: a share finishes on every last answer, keeps the shelf, and resumes after a reload.
- F6: a second tab never writes over the private board; it is read-only until the first closes.
- F8: the sv path is bounded.
- F9: per-board load cells, an unload grace period and a join rate.
- F10a: empty layers are reclaimed, new ones are limited, and a full board says so.
- F10b: the view link is never the edit key.
- F11: a resend is confirmed only while it is on the sheet in use.
- F12: a slow link no longer loops on a busy board.
- F14: `make test` runs the page tests.
- F15: the text-annotation test waits for the undo.
- F20: seats are kept for contributors.
- F21: whole-sheet broadcasts are charged to the board and the address.
- F22: IPv6 rate limits are keyed by /56, not /64.
- F23: a storage quota; new strokes and boards are refused near it.
- F25: the build image is pinned and a dirty tree does not ship.
- F26: a deploy rolls back to the previous relay when the new one is unhealthy.
- F27e: numbers just above a power of two read back exactly.
- F27g: bidi controls and invisible characters are taken out of names and text.
- F28: a reconnect snapshot leaves my strokes alone (test).
- F29: crumpling a blank private board does nothing.

### Waived

- F7 (relay memory under load): waived by operator, 2026-11-22. The relay unit restarts
  without limit; the memory budget already committed stays.
- F24 (any editor can wipe a board's history by crumpling six times fast): waived by
  operator, 2026-11-22.
  This is by design.

## v0.1.0

The JavaScript page and the Rust relay.
