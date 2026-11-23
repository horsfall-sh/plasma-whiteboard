# Changelog

## v0.3.0 (unreleased)

Shapes and arrows. The wire protocol gains one optional field on items, `sh` (see PROTOCOL.md).
Run the new relay with the new page: the relay checks `sh`, stores it, and adds its column to an
existing database by itself.

- **Shapes:** one toolbar menu holds Square, Circle, Triangle and Arrow. Drag to place a shape,
  filled or outlined (Filled, in the same menu) in the current colour and thickness. Drag one of
  your own shapes with the shape tool to move it. Shapes sync like strokes and undo the same way.
- **Arrows** are straight lines. Drag an end near a shape to see its anchors, one per side; let go
  on one to attach. Rest on a side for about a second and a half, then move, and it opens into
  finer anchors along that side. An attached end stays on its spot when either shape moves.
- **Arrowheads:** None, End, Start or Both, from their own toolbar menu.
- **Line jumps:** where an arrow crosses an earlier one, it hops over it with a small semicircle
  that scales with its thickness. Freehand strokes do not hop.
- **Thickness** is three icons instead of S, M and L. The widths are the same.
- **Share asks first.** "Share this board" opens a question saying that sharing connects the board
  to the relay server outside this browser and that anyone with the link can see and edit it.
  Cancel or Esc keeps the board local, and nothing is sent until Share is pressed.

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

From the council's pass 3:

- P3-A: a share the relay opens view-only no longer empties the private board. The share
  stops, the private board stays whole, and the page says why.
- P3-B: one address may hold only an eighth of the relay's memory budget in copies of a board
  being sent to its joins (its first join may always try), so another address still joins;
  a join refused for memory spends none of the address's new-layer allowance; and a join
  must send its whole snapshot within 30 seconds plus its size at 256 kB a second, or it is
  dropped and its charge given back.

### Waived

- F7 (relay memory under load): waived by operator, 2026-11-22. The relay unit restarts
  without limit; the memory budget already committed stays.
- F24 (any editor can wipe a board's history by crumpling six times fast): waived by
  operator, 2026-11-22.
  This is by design.

### Known limitations / deferred

From the council's passes 1 to 3. Each is minor or a nit and stays open in this release.

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
- P3-B: a sender with several addresses can still make joins busy; the per-address share only
  stops a single address. Each address's first join may hold a copy of any size, and an IPv6
  /48 holds 256 addresses as the relay counts them.
- P3-B: behind a shared address (a classroom), while one join is still catching up on a board
  with large live strokes, or a large board being edited, other joins from that address are
  told the relay is busy until it finishes. A sheet that changes while an older build of it is
  still being sent charges the address for one more build, up to 16 MB. A quiet board with no
  live strokes costs a join almost nothing, so this is rare.
- P3-B: an address is charged for every extra build of a sheet its joins are sent, even one it
  only shares with another address's join. This errs safe, but makes the busy answer above a
  little more likely.
- H5: the per-address storage share refills over the board lifetime, so one heavy user behind a
  shared address (a mobile carrier, a school) can stop everyone there from storing strokes for
  weeks. Undo and crumple give nothing back.
- Behind a shared address everyone also shares the join allowance (60, refilled at 2 a second)
  and the new-layer allowance (60, refilled at one every 30 seconds).
- P3-B: a link slower than about 2 Mbit/s cannot open a very large board: its join is
  dropped as too slow and reconnects. The deadline counts only the snapshot, not the strokes
  sent to the join while it catches up (up to 8 MB), so on a large busy board a somewhat faster
  link can be dropped too.
- G4: reloading `/b/<edit>` mid-upload resumes the share only on the next visit to `/`.
  Nothing is lost.
- G7: behind a shared address, one person can empty the new-layer allowance by joining and
  leaving boards, and keep it empty with about 2 joins a minute; a new board's creator there
  then joins view-only. A share made then stops with a message, and the private board is kept
  whole (P3-A).
  Dissent: the security reviewer on the council's pass-3 panel rates G7 major, not a known
  limitation. One person behind a shared address (NAT, an office, family Wi-Fi) can drain the
  new-layer allowance for everyone there, and a creator seated view-only sees their drawing
  vanish from the board, though it is still kept locally.
- A transient database error while a board is looked up reads as "gone", so a share resumed
  then is abandoned. The private board is kept, but the edit link to the half-filled board is
  forgotten.
- A share resumed when the page loads runs without the private board's writer lock. The other
  guards hold, so the worst case is a stroke kept twice on the private board.
- Every page, a shared board included, takes the private board's writer lock, so while one is
  open another tab on `/` is read-only.
- H1: in edge cases the share's toast can count strokes as not shared that were; they are
  kept on the private board as well.
- F17: a share's upload is quadratic in the number of strokes, since each confirm shifts the
  pending list (about 0.4 s for 10,000); no user cost was found.
- serde_json's `preserve_order` pulls in `indexmap`: a size and dependency cost only.
- The private board's save reads the whole of `wb.local` before its signature check, every 1.5
  seconds.
- Writing a number to the wire costs two formats and a parse.
- If a guard stops a share's last write to the private board, the write is dropped without a
  message and the share is forgotten, so a later share can upload the same strokes again.
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
