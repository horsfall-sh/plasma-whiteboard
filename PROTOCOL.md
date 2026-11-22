# Whiteboard wire protocol (v1)

The contract between the page (`web/`) and the relay (`relay/`). Change both sides together.

## Links and keys

- A board has two keys. Each is 24 characters of base64url (144 bits).
  - **Edit key:** random, made when the board is created.
  - **View key:** `base64url(HMAC-SHA256(key = edit key, msg = "wb-view-v1"))`, first 24 characters.
    An edit key gives the view key; a view key gives nothing else.
- The database stores only SHA-256 hashes of both keys.
- Link form: `https://host/b/<key>`. The same page serves both; the relay decides the role from the key.
- `POST /api/boards` (no body) returns `201 {"edit":"<key>","view":"<key>"}`. `429` when one address makes
  boards too fast, `503` when storage is near its quota.
- `GET /` serves the page, which calls `POST /api/boards` and moves to `/b/<edit key>` with
  `history.replaceState` (no reload).

## Text annotations

A text annotation is stored in the same log as strokes and belongs to its author's layer, so hide, delete,
restore, undo and ordering work the same way. It is never edited in place: the page edits one by undoing it
and sending a new one. The page draws it in a sans-serif font, line height 1.25, anchored at its top left.

Zoom and pan are local to each browser and never go on the wire.

## Crumple and the shelf

A board has one sheet in use and a shelf of up to five crumpled sheets. Crumpling renumbers the sheet's
strokes from sheet 0 to a new sheet number; nothing is copied or rewritten. A sixth crumple deletes the
oldest shelved sheet for good. Undo and layer delete or restore act on the sheet in use only. The limit of
10,000 strokes counts the sheet in use and the shelf together.

## Coordinates

Points are flat arrays of `x, y, pressure` triples. The unit is the width of one page. The centre page is `x` 0 to 1, `y` 0 to 0.5625; the surface is a 3 by 3 grid of pages around it, so `x` runs from -1 to 2 and `y` from -0.5625 to 1.125. Pressure is 0 to 1. The relay clamps points to these ranges.
The relay clamps values to those ranges and writes them back out rounded to 5 decimals.

## WebSocket

`GET /ws`, text frames, one JSON object per frame, the field `t` names the message. Frames over 1 MiB close
the connection. The key is sent in the first message, never in the URL.

### Client to relay

| `t` | Fields | Meaning |
| --- | --- | --- |
| `join` | `key`, `who` | Must be the first message, within 10 s. `who` is the browser's contributor secret for this board (16 to 64 characters of `[A-Za-z0-9_-]`), kept in `localStorage`. Viewers send `who: null`. |
| `b` | `id`, `c`, `w` | Begin a live stroke. `id`: 1 to 40 characters of `[A-Za-z0-9_-]`, unique in the board. `c`: `#rrggbb`. `w`: width in board units, over 0 and at most 0.2. |
| `p` | `id`, `pts` | More points for my live stroke. |
| `e` | `id`, `c`, `w`, `pts` | End my live stroke. `pts` is the final simplified point list. If it is missing or empty the relay keeps the points it collected from `p`. `c` and `w` repeat the values from `b`, so that when the relay has no live stroke with this `id` (the stroke began while offline) it stores the stroke as if it were an `s`. |
| `s` | `id`, `c`, `w`, `pts`, `txt`? | A whole finished stroke in one message. With `txt` it is a **text annotation**: `pts` is then exactly one point (the top left of the text), `w` is the font size in board units, and `txt` is 1 to 500 characters on at most 20 lines, with no control characters other than newline. Used to resend strokes that were not confirmed before a disconnect. Sending an `id` the relay already holds from the same layer is confirmed again with `ok` and not stored twice. |
| `u` | `id` | Undo one of my own strokes. |
| `ld` | `l` | Delete layer `l` for everyone (any editor). |
| `lr` | `l` | Restore the deleted strokes of layer `l` (any editor). |
| `cr` | | Crumple: move everything on the sheet in use to the shelf and start blank (any editor). Ignored on a blank sheet. |
| `un` | `k` | Bring crumpled sheet `k` back (any editor). The sheet in use, if it has anything on it, is crumpled in its place. |
| `sv` | `k` | Ask for the strokes of crumpled sheet `k`, to draw a preview. |
| `nm` | `n`, `c` | My label: a display name (1 to 24 characters) and my colour `#rrggbb`. Kept in memory only while the board is open; the browser sends it after every `hello`. |
| `pg` | `pts` | An attention ping: a shape (2 to 300 points) shown to everyone for a few seconds. Never stored. One every two seconds per contributor; extra ones are dropped. |

Messages from a viewer other than `join` are ignored.

### Relay to client

| `t` | Fields | Meaning |
| --- | --- | --- |
| `hello` | `role`, `me`, `view`, `layers`, `shelf` | Sent after a good `join`. Each layer is `{l, c, del, n}` (`n`: name or null). `role` is `edit` or `view`. `me` is my layer number (null for viewers). `view` is the view key: sent to every edit-key holder, including one joined as a viewer because the board had no room for another contributor; null for view-key holders. `layers` is `[{l, c, del}]`: layer number, assigned colour, and whether it holds deleted strokes that can be restored. `shelf` is `[{k, n}]`: the crumpled sheets, oldest first, with their sheet number and item count. **The client drops all board state when it receives this**; the snapshot follows. |
| `ss` | `a` | Snapshot chunk: an array of finished items `{id, l, q, c, w, pts}` in `q` order. A text annotation also has `txt`. |
| `ready` | | Snapshot complete. The client now resends unconfirmed strokes with `s`. |
| `b` | `id`, `l`, `c`, `w` | Someone began a live stroke. |
| `p` | `id`, `pts` | Points for a live stroke. |
| `e` | `id`, `l`, `q`, `c`, `w`, `pts`, `txt`? | A stroke or text annotation was committed. If a live stroke with this `id` exists it is replaced. Not sent to the author. |
| `ok` | `id`, `q` | To the author: the stroke is committed to storage, with sequence number `q`. It is sent only after the commit, so the page may forget its own copy. A stroke that fails to store gets no `ok`: the board ends every connection with `busy`, and the page resends it on reconnect. |
| `no` | `id`, `m` | To the author: the stroke was refused (board full, server out of memory, storage full, bad data). The client removes it. |
| `u` | `id` | A stroke was removed (undone, or a live stroke abandoned). Not sent to the author of an undo. |
| `l` | `l`, `c` | A new layer (a new contributor joined). Sent to everyone. |
| `ld` | `l` | Layer `l` was deleted: drop its finished strokes and mark it restorable. Sent to everyone, including the sender. |
| `lr` | `l`, `a` | Layer `l` was restored: `a` holds its strokes; insert them by `q`. Sent to everyone. |
| `cr` | `k`, `by`, `drop`, `shelf` | The sheet was crumpled into shelf entry `k` by layer `by`: drop every stored item (keep your own unconfirmed ones; they land on the new sheet) and clear every layer's `del`. `drop` is the sheet number thrown away to keep the shelf at five, or null. Sent to everyone. |
| `un` | `k`, `by`, `shelf`, `layers`, `a` | Sheet `k` was brought back: `a` holds its items, `layers` the layer list as in `hello`. Replace the board with them. Sent to everyone. |
| `sv` | `k`, `a` | To the one who asked: the items of crumpled sheet `k`. |
| `n` | `n`, `on` | Number of connections on the board now, and the layers that have one open. |
| `nm` | `l`, `n`, `c` | Layer `l` now has this name and colour. Sent to everyone. Also sent (`n` null, the layer's first colour) when an empty layer no one is on is handed to a new contributor. |
| `pg` | `l`, `k`, `pts` | A ping from layer `l` with letter `k` (A to Z, then round again). Sent to everyone, including the sender. |
| `err` | `code`, `m` | Fatal; the relay closes after it. `gone`: no such board (do not reconnect). `busy`: too many connections, the server at capacity, or the board could not be read or written (retry later). `rate`: sending too fast. `slow`: the connection fell too far behind the board's broadcasts to keep up (wait the longest before reconnecting). `bad`: malformed join. Not fatal: `full`, sent right after `hello` when an edit link joins as a viewer because the board has no room for another contributor (or the address was given too many new layers lately); the page says so and stays connected. |

`q` is a per-board sequence number. Strokes are drawn in `q` order. A stroke is never changed after it is
stored; undo, layer delete and restore only flip its state.

### Ordering

The relay handles one board's messages one at a time and broadcasts in that order, so every client sees the
same sequence. What is broadcast while a joining client's snapshot is still being sent is held for it (up to 8 MB) and sent right after, so a slow link is not dropped just for joining a big, busy board. A client that falls too far behind is disconnected with `slow` and gets a fresh snapshot when it reconnects; the page waits from the shortest delay again only after a connection has stayed ready for 30 s.

## Limits

| Limit | Value |
| --- | --- |
| Message size | 1 MiB |
| Points per stroke | 20,000 |
| Text annotation | 500 characters, 20 lines |
| Strokes kept per board (sheet in use plus shelf, visible plus restorable) | 10,000 |
| Crumpled sheets on the shelf | 5 |
| Stroke data kept per board (sheet in use plus shelf) | 16 MB |
| Bytes per connection | burst of 2 MB, then 256 KB a second |
| Whole-sheet operations per connection (`cr`, `un`, `sv`, `lr`) | burst of 12, then one every 2 s; over the limit they are ignored |
| Live strokes per board / per connection | 64 / 4 |
| Contributors per board | 200; when all are taken, an empty layer no one is on is handed to the next one, else it joins as a viewer and is told (`full`) |
| New contributor layers per address | burst 60, then one per 30 s (over it, the browser joins as a viewer) |
| Connections per board / per address | 100 / 40; viewers may hold at most 80 of a board's 100 |
| Messages per connection | 300 per second sustained, burst 600 |
| New boards per address | burst 10, then one per 10 s |

An "address" is one IPv4 address, or one IPv6 /56 (all of a home's networks share it).
