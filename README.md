# Whiteboard

A live shared whiteboard that works like a pastebin. Opening the site makes a board with an unguessable
link. Anyone holding the link can draw on it in real time. Draw with a pen on a tablet, open the same
board in a browser window on a laptop, and share that window in a video call.

- Each board has two links: an **edit link** and a **view-only link**. The link is the key; there are no
  accounts.
- Each browser is a contributor with its own colour and its own layer.
- Hiding a layer affects only your browser. Deleting a layer affects everyone and can be undone.
- Boards are stored in SQLite and survive a restart.

## Parts

| Path | What |
| --- | --- |
| `relay/` | The relay: one Rust binary (axum, tokio, SQLite in WAL mode). Serves the page, makes boards, relays strokes. The page is embedded in the binary. |
| `web/` | The page: plain HTML, CSS and JS. No framework and no build step. |
| `smooth/` | Stroke simplification in Rust (Ramer-Douglas-Peucker over x, y, pressure), built to WebAssembly. The page falls back to the same algorithm in JS if the module does not load. |
| `PROTOCOL.md` | The wire contract between page and relay, and the limits. |
| `BRIEF.md` | The build brief. |

## Build and run

Needs Rust with the `wasm32-unknown-unknown` target (`rustup target add wasm32-unknown-unknown`).

    make test     # smooth tests, relay unit and end-to-end tests, JS syntax check
    make run      # http://127.0.0.1:8787, page served from web/ so edits show on reload
    make relay    # release binary at relay/target/release/wb-relay
    make linux    # static Linux x86-64 binary in relay/target-linux/release/ (needs Docker)

Open `http://127.0.0.1:8787/`. The address bar changes to `/b/<edit key>`. The **Share** button shows both
links. The status line at the bottom right shows the connection, your role, the simplifier in use (`wasm` or
`js`) and how many browsers are on the board.

## Settings

Environment variables for `wb-relay`:

| Variable | Default | Meaning |
| --- | --- | --- |
| `WB_ADDR` | `127.0.0.1:8787` | Listen address. |
| `WB_DB` | `whiteboard.db` | SQLite file. |
| `WB_TTL_DAYS` | `90` | A board not opened for this many days is deleted. `0` keeps boards for ever. |
| `WB_TRUST_PROXY` | unset | Set to `1` behind a reverse proxy you run, so rate limits use the last `X-Forwarded-For` address. Leave unset when the relay faces the network directly. |
| `WB_WEB_DIR` | unset | Serve the page from this directory instead of the copy in the binary (development). |

The relay speaks plain HTTP. Put it behind a TLS proxy (Caddy) for anything beyond your own machine. The
page uses `wss:` on its own when served over HTTPS.

## How it works

- **Keys.** The edit key is 144 random bits. The view key is derived from it with HMAC-SHA256, so an edit
  link can show the view link but not the reverse. The database holds only hashes of both.
- **Identity.** On an edit link the browser makes a random secret for that board and keeps it in
  `localStorage`. The relay maps it to a layer number and assigns the colour. The same browser gets the same
  layer back after a reload or a relay restart. Clearing site data makes a new contributor.
- **Strokes.** Points are vectors in board-width units (x 0 to 1, y 0 to 0.5625). The page shows a stroke
  live while it is drawn, simplifies it when the pen lifts, and sends the final points. The relay gives each
  stored stroke a sequence number; clients draw in that order. Strokes are never edited: undo, layer delete
  and restore flip a state flag.
- **Offline.** Strokes drawn while disconnected are kept and sent when the connection returns.
- **Ordering.** The relay handles one board's messages one at a time and broadcasts in that order.
- **Storage.** One thread owns the SQLite connection and batches writes into transactions, so drawing never
  waits on disk. A crash can lose the last few milliseconds of strokes.

## Limits

See the table at the end of `PROTOCOL.md`. The main ones: 10,000 strokes per board, 20,000 points per
stroke, 200 contributors per board, 100 connections per board.

## Not yet proven

- **A real pen.** Pressure, palm rejection (finger touches are ignored once a pen has been seen) and the
  pen's eraser end have only been exercised with a mouse and scripted pointer events.
- **A slow tablet.** Redraws are time-sliced at 6 ms per frame and the device pixel ratio is capped at 2,
  but this has not been measured on real hardware.
