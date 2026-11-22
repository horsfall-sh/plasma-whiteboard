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

From the council's pass 2:

- H1: a share keeps the strokes the relay refused on the private board, including a live
  stroke drawn during the upload and refused then.
- H2: a share cut short to a board that has since gone returns to the private board at `/`.
- H3: only the tab that writes the private board can share it, and a share's last write keeps
  the guards for a damaged or newer board.
- H4: every join's copy of the board is charged to the memory budget until it is sent: each
  build of the sheet's snapshot a slow join keeps alive, and each join's copy of the live
  strokes. A join that cannot be charged is told the relay is busy.
- H5: one address may hold only a sixteenth of storage, refilled over the board lifetime.
- G5: a share keeps the private board's lock until its last write, so a waiting tab takes
  over only after it.
- G6: a text item with fewer than two numbers no longer stops the page, and any panic shows a
  "something broke" card with a Reload link instead of a page that has stopped silently.
- The relay unit restarts on any exit (`Restart=always`).

### Waived

- F7 (relay memory under load): waived by operator, 2026-11-22. The relay unit restarts
  without limit; the memory budget already committed stays.
- F24 (any editor can wipe a board's history by crumpling six times fast): waived by
  operator, 2026-11-22.
  This is by design.

### Known limitations / deferred

From the council's passes 1 and 2. Each is minor or a nit and stays open in this release.

- F27a-c: rated negligible by the council; left as they are.
- F27f: PROTOCOL.md has drifted from the code in places; a documentation pass is deferred.
- F27h: a layer id can be taken by someone else after its holder leaves; minor, deferred.
- F27i: sheet numbers can be reused after a relay restart; minor, deferred.
- F27j: `issue-cert.sh` names the `eth0` interface; works on the current host, deferred.
- F27k: a malformed `WB_TRUST_PROXY` or `WB_TTL_DAYS` falls back to the default without
  saying so; deferred.
- F27l: the deploy has no SELinux step (partly refuted by the council); deferred.
- F27m: rated plausible but generic by the council; no change made.
- F27n: edit keys of boards a browser has opened build up in its localStorage; deferred.
- H5: the per-address storage share is kept in memory, so a relay restart refills it.
- H5: one network with many addresses (an IPv6 /48, or 16 IPv4 addresses) can still fill
  storage.
- G4: reloading `/b/<edit>` mid-upload resumes the share only on the next visit to `/`.
  Nothing is lost.
- G7: behind a shared address, after about 60 new contributors in about 30 minutes, a new
  board's creator may join view-only. One seat refills every 30 seconds.
- G10: until the relay's hello arrives, the share panel's view-only link shows the edit link.
- G11: if you cancel an edit of your own text before its ack, it draws on top locally until
  reload.
- G12: a name the relay rejects (C1 controls) or that is over the rate limit is dropped with no
  message.
- G13: a layer you hid stays hidden for you if its number is reclaimed by a new person.
- G14: if the browser's crypto fails, the contributor secret is a constant (theoretical).
- G16: undo, layer delete and crumple are broadcast before commit, so a crash within the write
  batch can bring them back.
- G17: the parity test passes silently when git history or node is missing.
- G18: the deploy auto-rollback checks only `/healthz`, which does not load the page;
  `rollback.sh` is manual.
- The people panel can cover the people button on a narrow screen, as in v0.1.0; the panel's
  Close button shuts it.
- F30: a null in a number list on the wire reads as 0, as the JavaScript page did; this is
  documented in the code and kept.

Resolved by the port itself: F27o (the name and colour are kept in `wb.name` and `wb.hue`)
and F27p (`wb-crumple` is linked into the page).

## v0.1.0

The JavaScript page and the Rust relay.
