# Roadmap

What comes next for Plasma Whiteboard, in order. Each item says where the project is today, what changes, and
what it depends on. Item 3 is done in v0.2.0; nothing else here is built yet. The design history is in [`DESIGN-NOTES.pdf`](DESIGN-NOTES.pdf).

**Direction: by default, the server is a single binary with no database.** It serves the page and relays
encrypted traffic between the browsers on a board. Saving boards on the server becomes an opt-in build
feature (item 4). Today's relay keeps boards in SQLite; items 1 and 4 together replace that.

| # | Item | Depends on |
| --- | --- | --- |
| 1 | [End-to-end encryption](#1-end-to-end-encryption) | — |
| 2 | [TLS in the relay](#2-tls-in-the-relay) | — |
| 3 | [The whole client in Rust](#3-the-whole-client-in-rust) (done in v0.2.0) | — |
| 4 | [Saved boards (opt-in)](#4-saved-boards-opt-in) | 1 |
| later | [Scene model and export, agent collaboration, voice input](#later) | 1 |

## 1. End-to-end encryption

**Today:** the relay reads every stroke, stores it in SQLite, and serves it to anyone with a link. The edit and
view keys in the link are bearer secrets the relay checks.

**Goal:** the relay never sees board content, and keeps none.

- **Boards do not persist on the server.** A board lives in the browsers that have it open. The relay holds
  only the connections and orders the messages between them. Item 4 adds opt-in, encrypted saving.
- **The link carries the key, the relay never does.** A share link is a session key in the URL fragment (the
  part after `#`, which a browser never sends to a server), plus a separate **one-time join token**. The token
  admits one browser and is burned on first use. Whoever redeems it first gets in. That means one link per person, where today one link serves everyone.
- **View-only links still work.** A view link admits a browser that can decrypt and watch but not draw. The
  relay refuses any board traffic sent from a view-token peer, without reading its content.
- **The crumple shelf and history live only in the browsers.** A crumpled page can be brought back by any
  browser that still has it, never from the server.
- Private boards (nothing leaves the browser until Share) stay as they are.

Open design questions: how the group key reaches a newcomer (wrapped to the newcomer's key pair by a browser
already on the board), rekeying when someone is removed, and what a newcomer sees when no other browser is
online to hand over the board. `DESIGN-NOTES.pdf` section 2 has the first draft.

## 2. TLS in the relay

**Today:** the relay speaks plain HTTP and needs nginx or Caddy in front of it for TLS (`deploy/`).

**Goal:** one binary terminates TLS itself, with no proxy in front.

- TLS with `rustls`.
- Certificates are issued and renewed **in the relay process** over ACME (Let's Encrypt or any ACME CA). There is
  no cron job, certbot or proxy.
- Plain-HTTP mode stays for local development and for anyone who still wants their own proxy.
- `deploy/` drops the nginx and Caddy files and `issue-cert.sh`.

## 3. The whole client in Rust

**Done in v0.2.0.** The page is now Rust in `page/`, and `web/app.js` is gone; see [`CHANGELOG.md`](CHANGELOG.md).
The plan below is kept as it was written.

**Before v0.2.0:** the page logic is `web/app.js` (about 1,900 lines) and `web/app.css` (about 220). Only stroke
simplification (`smooth/`) and the crumple mesh (`crumple/`) are Rust compiled to WebAssembly.

**Goal:** all page logic in Rust compiled to WebAssembly, behaving exactly as the current page does: private
until Share, view-only links, finger mode, smoothing, labels, pings, layers, crumple and the shelf.

**Proposed design:**

- **Layout.** A new crate `page/` (cdylib plus rlib, `wasm-bindgen` and `web-sys`, drawing on canvas, with no UI
  framework). `smooth/` and `crumple/` become ordinary dependencies, so the separate `.wasm` fetches go away.
  - *Pure modules, tested on the host with `cargo test`:*
    - `proto`: every `PROTOCOL.md` message in both directions.
    - `model`: strokes in sequence order, layers, unconfirmed strokes, the offline undo queue, and the
      resend-on-reconnect rules.
    - `view`: zoom, pan and the page grid.
    - `geom`: line width, bounds, curves, colours, ping shapes.
    - `local`: the private board and its storage format.
  - *Browser modules:*
    - `render`: canvas 2D, with the 6 ms-per-frame redraw budget.
    - `input`: pen, finger mode, pinch, wheel and keys.
    - `net`: WebSocket with backoff, board creation, and the paced upload on Share.
    - `ui`: toolbar, panels, roster, shelf, share and the text editor.
    - `fx`: the WebGL crumple animation.
    - `storage`: `localStorage` with a memory fallback.
- **What stays JavaScript:** only the loader that `wasm-bindgen` generates (built by `make`, never hand-written)
  and a two-line `boot.js` that imports and starts it. The loader cannot be inline in `index.html`, because the
  page's Content Security Policy (`script-src 'self' 'wasm-unsafe-eval'`) refuses inline script, and the Rust page needs its `'wasm-unsafe-eval'` part, and that policy stays as it is.
  `index.html` stays a minimal shell.
- **CSS.** Generating CSS at runtime would mean injecting a `<style>` element, which the policy
  (`style-src 'self'`) also refuses. So `app.css` stays a static file, cut down to what the Rust page uses.
  Positions that change at runtime are set through the element's style from Rust, as `app.js` already does.
- **Build.** `make wasm` needs the `wasm32-unknown-unknown` target and `wasm-bindgen-cli` pinned to the crate's
  `wasm-bindgen` version. It checks the version and prints the install command if it is wrong.
- **No JavaScript fallback.** A browser without WebAssembly gets a short "this page needs WebAssembly" notice
  instead of a board.
- **Compatibility.** The wire format and the private board's `localStorage` format stay byte for byte the same,
  so boards saved by the current page still open.
- **Proving parity.**
  - Rust unit tests for every pure module, with cases taken from `app.js`.
  - A differential run in headless Chromium: the old page and the new page get the same scripted pointer and
    network session against a local relay. The run compares the frames sent, the storage written and the
    canvas pixels.
  - A hands-on checklist on a real tablet before release.
- **Size.** About 4,000 lines of Rust, reviewed in steps: protocol and model, view and geometry, private board,
  render, input, network, UI, crumple, then the switch-over. The JavaScript page stays live until the last step.

## 4. Saved boards (opt-in)

**Today:** every board is saved in SQLite and deleted after 90 days unopened by default (`WB_TTL_DAYS`; 0 keeps boards for ever).

**Goal:** saving is something an operator chooses to compile in and switch on, and even then the server cannot
read what it saves.

- **A cargo feature, `persist`, off by default.** The default relay binary contains no database code and no
  database dependency.
- **With `persist` compiled in, a runtime setting:** `off`, `per-board` (a board's owner chooses to save it), or
  `always`.
- **Stored boards are ciphertext only.** The browser encrypts them, and the key stays in the link fragment, so
  a breach of the server leaks nothing readable.
- **Expiry and size caps** are set by whoever runs the server.
- **A lost link means a lost board.** The server cannot recover it, by design.

Depends on item 1: there is nothing safe to store until the content is encrypted in the browser.

## Later

From the phases in `DESIGN-NOTES.pdf`, after the items above:

- **Scene model, shapes, export and import.** Every element has an id, a type, a position, a label and its
  connections. Adds boxes, arrows and text, and a structured drawing mode beside freehand. A board can be saved
  to a local file, and importing a file seeds a fresh board.
- **Agent collaboration.** An agent joins as another peer through a small headless client built from the same
  Rust protocol code. It reads the scene, adds elements on its own layer, and waits for pings. Board content is
  data to it, never instructions.
- **Voice input.** A ping opens a text box that the device's own dictation fills, so the page never handles
  audio and only encrypted text leaves the device.
