# Browser checks

Each script drives the page in a headless browser against a local relay and prints one
`PASS` or `FAIL` line per check. Run one with `tests/browser/run.sh <name>` after `make wasm`.

- `smoke`: the private board, a share, a second person, crumple and unfold, zoom and undo (Chromium).
- `panel`: the people panel typed into for real (Firefox).
- `h2`: a share cut short to a board that has since gone returns to `/` (Firefox).
- `g6`: a panic shows the "something broke" card with a Reload link (Firefox and Chromium).
  Remove `std::panic::set_hook` from `start()` in `page/src/web.rs` and it fails.

They are not part of `make test`: they need node, playwright-core and the browsers.
