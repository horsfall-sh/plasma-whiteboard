(() => {
'use strict';
// Coordinates are in board-width units: x in 0..1, y in 0..ASPECT. The same
// stroke therefore lands in the same place on every screen size.
const ASPECT = 9 / 16;
const EPS = 0.0004;          // simplification tolerance (under 1px on a 1600px board)
const PRESSURE_SPAN = 0.15;  // must match smooth/src/lib.rs
const ERASER_SCALE = 6;
const ERASER_COLOR = '#ffffff';
const FIXED_COLORS = ['#111111', '#d63031', '#0a66d8', '#1e9e4a'];
const COLOR_NAMES = { '#111111': 'Black', '#d63031': 'Red', '#0a66d8': 'Blue', '#1e9e4a': 'Green' };
const REDRAW_BUDGET_MS = 6;  // time spent redrawing finished strokes per frame
const MAX_ZOOM = 8;
const TEXT_SCALE = 5;        // font size of a text annotation = pen width x this
const LINE_HEIGHT = 1.25;
const FONT = '-apple-system, "Segoe UI", Roboto, Helvetica, Arial, sans-serif';

const $ = (s) => document.querySelector(s);
const el = (tag, cls, text) => {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  if (text != null) e.textContent = text;
  return e;
};

const boardEl = $('#board'), base = $('#base'), live = $('#live');
const bctx = base.getContext('2d');
const lctx = live.getContext('2d', { desynchronized: true }) || live.getContext('2d');
const spare = document.createElement('canvas'); // off-screen target for flicker-free redraws
const sctx = spare.getContext('2d');
const statusEl = $('#status'), statusText = $('#status span');
const toastEl = $('#toast'), msgEl = $('#msg');
const layersPanel = $('#layersPanel'), sharePanel = $('#sharePanel'), layerList = $('#layerList');

// ---- state -----------------------------------------------------------------

let bw = 1;                    // board width in CSS px
let key = '';                  // the key in the link
let role = 'view';             // 'edit' | 'view', from hello
let me = null;                 // my layer number (null for viewers)
let viewKey = null;
let people = 0;
let net = 'connecting';
let engine = 'js';

const strokes = [];            // finished strokes in q order; unconfirmed ones last
const byId = new Map();        // id -> finished stroke
const counts = new Map();      // layer -> number of finished strokes
const liveStrokes = new Map(); // id -> stroke in progress (local or remote)
const layers = new Map();      // l -> { l, c, del }
const pending = new Map();     // id -> my finished stroke the relay has not confirmed
const mine = [];               // ids of my finished strokes, for undo
let undoQueue = [];            // undos made while offline
let hidden = new Set();        // layers hidden in this browser only

// The view: zoom 1 shows the whole board; pan is the board point at the top
// left of the screen, in board units. Local to this browser.
let zoom = 1, panX = 0, panY = 0;
let dpr = 1;
let rv = { z: 1, x: 0, y: 0 };  // the view the base canvas was last drawn at
let tool = 'pen';              // 'pen' | 'erase' | 'text'

let color = null;              // selected pen colour (null until hello gives mine)
let myColor = null;
let width = 0.004;

// ---- storage (may be blocked: fall back to memory) -------------------------

const mem = new Map();
function lsGet(k) {
  try { const v = localStorage.getItem(k); if (v != null) return v; } catch { /* blocked */ }
  return mem.has(k) ? mem.get(k) : null;
}
function lsSet(k, v) {
  mem.set(k, v);
  try { localStorage.setItem(k, v); } catch { /* blocked */ }
}

function contributorSecret() {
  const k = 'wb:who:' + key;
  let who = lsGet(k);
  if (!who || !/^[A-Za-z0-9_-]{16,64}$/.test(who)) {
    const b = crypto.getRandomValues(new Uint8Array(24));
    who = btoa(String.fromCharCode(...b)).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '');
    lsSet(k, who);
  }
  return who;
}
function loadHidden() {
  hidden = new Set();
  try {
    const a = JSON.parse(lsGet('wb:hide:' + key) || '[]');
    if (Array.isArray(a)) for (const l of a) if (Number.isInteger(l)) hidden.add(l);
  } catch { /* ignore a bad value */ }
}
function saveHidden() { lsSet('wb:hide:' + key, JSON.stringify([...hidden])); }

// ---- simplification: Rust/WASM if smooth.wasm is served, else JS -----------

let wasm = null;
(async () => {
  try {
    const res = await fetch('/smooth.wasm');
    if (!res.ok) return;
    const { instance } = await WebAssembly.instantiate(await res.arrayBuffer(), {});
    const x = instance.exports;
    if (x.wb_alloc && x.wb_simplify && x.wb_free && x.memory) { wasm = x; engine = 'wasm'; showStatus(); }
  } catch { /* stay on the JS fallback */ }
})();

// Round to 5 decimals so points stay short on the wire (f32 values print long).
const r5 = (v) => Math.round(v * 1e5) / 1e5;

function simplify(pts, eps) {
  const n = pts.length / 3;
  if (n <= 2) return pts;
  if (wasm) {
    try {
      const ptr = wasm.wb_alloc(n * 3);
      new Float32Array(wasm.memory.buffer, ptr, n * 3).set(pts);
      const m = wasm.wb_simplify(ptr, n, eps);
      // Re-read the buffer: memory may have grown during the call.
      const out = Array.from(new Float32Array(wasm.memory.buffer, ptr, m * 3), r5);
      wasm.wb_free(ptr, n * 3);
      return out;
    } catch { wasm = null; engine = 'js'; showStatus(); }
  }
  return simplifyJs(pts, eps);
}

// Same algorithm as smooth/src/lib.rs (iterative Ramer-Douglas-Peucker).
function simplifyJs(pts, eps) {
  const n = pts.length / 3, e2 = eps * eps, ps = eps / PRESSURE_SPAN;
  const keep = new Uint8Array(n); keep[0] = keep[n - 1] = 1;
  const stack = [0, n - 1];
  while (stack.length) {
    const b = stack.pop(), a = stack.pop();
    if (b <= a + 1) continue;
    const ax = pts[a * 3], ay = pts[a * 3 + 1], ap = pts[a * 3 + 2];
    const dx = pts[b * 3] - ax, dy = pts[b * 3 + 1] - ay, dp = pts[b * 3 + 2] - ap;
    const len2 = dx * dx + dy * dy;
    let best = 0, idx = a;
    for (let i = a + 1; i < b; i++) {
      const px = pts[i * 3] - ax, py = pts[i * 3 + 1] - ay;
      const t = len2 <= 1.2e-7 ? 0 : Math.min(1, Math.max(0, (px * dx + py * dy) / len2));
      const qx = px - t * dx, qy = py - t * dy, pe = (pts[i * 3 + 2] - (ap + t * dp)) * ps;
      const d2 = qx * qx + qy * qy + pe * pe;
      if (d2 > best) { best = d2; idx = i; }
    }
    if (best > e2) { keep[idx] = 1; stack.push(a, idx, idx, b); }
  }
  const out = [];
  for (let i = 0; i < n; i++) if (keep[i]) out.push(pts[i * 3], pts[i * 3 + 1], pts[i * 3 + 2]);
  return out;
}

// ---- rendering -------------------------------------------------------------
// Two stacked canvases: `base` holds finished strokes, `live` holds strokes in
// progress. A finished stroke is stamped onto `base` once, so drawing cost
// stays flat however full the board gets.

function lineW(s, p) { return Math.max(0.6, s.w * bw * (0.4 + 1.2 * p)); }

function prep(ctx, s) { ctx.strokeStyle = ctx.fillStyle = s.c; ctx.lineCap = ctx.lineJoin = 'round'; }

function dot(ctx, s) {
  const P = s.pts;
  ctx.beginPath(); ctx.arc(P[0] * bw, P[1] * bw, lineW(s, P[2]) / 2, 0, 6.2832); ctx.fill();
}

// Draw the curve piece that ends at the midpoint between points i-1 and i.
function seg(ctx, s, i) {
  const P = s.pts, j = (i - 1) * 3, k = i * 3;
  const x1 = P[j] * bw, y1 = P[j + 1] * bw, x2 = P[k] * bw, y2 = P[k + 1] * bw;
  ctx.lineWidth = lineW(s, (P[j + 2] + P[k + 2]) / 2);
  ctx.beginPath();
  if (i === 1) { ctx.moveTo(x1, y1); }
  else { ctx.moveTo((P[j - 3] * bw + x1) / 2, (P[j - 2] * bw + y1) / 2); }
  ctx.quadraticCurveTo(x1, y1, (x1 + x2) / 2, (y1 + y2) / 2);
  ctx.stroke();
}

// Finished strokes are simplified, so their points are sparse and already
// within EPS of the pen path: draw them as straight pieces. Consecutive pieces
// of the same width share one path, so a constant-pressure stroke (mouse,
// finger) costs a single stroke() call.
function drawFull(ctx, s, v) {
  const P = s.pts, n = P.length / 3;
  if (!n) return;
  if (v && v.z > 1 && offScreen(s, v)) return;
  if (s.txt != null) return drawText(ctx, s);
  prep(ctx, s);
  if (n === 1) return dot(ctx, s);
  let lw = lineW(s, (P[2] + P[5]) / 2);
  ctx.lineWidth = lw;
  ctx.beginPath(); ctx.moveTo(P[0] * bw, P[1] * bw);
  for (let k = 3; k < P.length; k += 3) {
    const w = lineW(s, (P[k - 1] + P[k + 2]) / 2);
    if (w !== lw) {
      ctx.stroke();
      lw = w; ctx.lineWidth = lw;
      ctx.beginPath(); ctx.moveTo(P[k - 3] * bw, P[k - 2] * bw);
    }
    ctx.lineTo(P[k] * bw, P[k + 1] * bw);
  }
  ctx.stroke();
}

const textLines = (s) => s.txt.split('\n');

function drawText(ctx, s) {
  const px = s.w * bw, lh = px * LINE_HEIGHT;
  ctx.font = `${px}px ${FONT}`; ctx.fillStyle = s.c; ctx.textBaseline = 'middle'; ctx.textAlign = 'left';
  const lines = textLines(s);
  for (let i = 0; i < lines.length; i++) ctx.fillText(lines[i], s.pts[0] * bw, s.pts[1] * bw + (i + 0.5) * lh);
}

// Bounding box in board units, worked out once (items never change).
function bbox(s) {
  if (s.bb) return s.bb;
  const P = s.pts;
  let x0 = P[0], x1 = P[0], y0 = P[1], y1 = P[1];
  if (s.txt != null) {
    const px = 100; // measure at a fixed size, then scale
    bctx.save(); bctx.font = `${px}px ${FONT}`;
    let wmax = 0;
    for (const line of textLines(s)) wmax = Math.max(wmax, bctx.measureText(line).width);
    bctx.restore();
    x1 = x0 + (wmax / px) * s.w; y1 = y0 + textLines(s).length * LINE_HEIGHT * s.w;
  } else {
    for (let k = 3; k < P.length; k += 3) {
      if (P[k] < x0) x0 = P[k]; else if (P[k] > x1) x1 = P[k];
      if (P[k + 1] < y0) y0 = P[k + 1]; else if (P[k + 1] > y1) y1 = P[k + 1];
    }
    const pad = s.w; x0 -= pad; y0 -= pad; x1 += pad; y1 += pad;
  }
  return (s.bb = [x0, y0, x1, y1]);
}
function offScreen(s, v) {
  const b = bbox(s);
  return b[2] < v.x || b[3] < v.y || b[0] > v.x + 1 / v.z || b[1] > v.y + ASPECT / v.z;
}

// ---- the view (zoom and pan) -------------------------------------------------

const curView = () => ({ z: zoom, x: panX, y: panY });
function setCtxView(ctx, v) {
  const k = dpr * v.z;
  ctx.setTransform(k, 0, 0, k, -v.x * bw * k, -v.y * bw * k);
}
function wipe(ctx) {
  ctx.save(); ctx.setTransform(1, 0, 0, 1, 0, 0);
  ctx.clearRect(0, 0, ctx.canvas.width, ctx.canvas.height); ctx.restore();
}
// Until the base canvas is redrawn at the current view, stretch what it holds
// with a CSS transform. That costs nothing per frame, so pinch stays smooth
// however full the board is.
function stretchBase() {
  if (rv.z === zoom && rv.x === panX && rv.y === panY) { base.style.transform = ''; return; }
  const f = zoom / rv.z;
  base.style.transform = `translate(${(rv.x - panX) * bw * zoom}px, ${(rv.y - panY) * bw * zoom}px) scale(${f})`;
}
let viewTimer = 0;
// Change the view. `settle`: redraw sharply now (end of a gesture, a button);
// otherwise shortly after the last change (wheel).
function setView(z, x, y, settle) {
  z = Math.min(MAX_ZOOM, Math.max(1, z));
  if (z < 1.02) z = 1;
  x = Math.min(1 - 1 / z, Math.max(0, x));
  y = Math.min(ASPECT * (1 - 1 / z), Math.max(0, y));
  const same = z === zoom && x === panX && y === panY;
  zoom = z; panX = x; panY = y;
  if (!same) { stretchBase(); redrawLive(); placeEditor(); showZoom(); }
  clearTimeout(viewTimer);
  if (settle) settleView(); else viewTimer = setTimeout(settleView, 140);
}
function settleView() {
  clearTimeout(viewTimer);
  if (rv.z !== zoom || rv.x !== panX || rv.y !== panY) redrawBase();
}
// Zoom by factor f keeping the screen point (sx, sy) (px from the board's
// top left) over the same board point.
function zoomAt(f, sx, sy, settle) {
  const z = Math.min(MAX_ZOOM, Math.max(1, zoom * f));
  const bx = panX + sx / (bw * zoom), by = panY + sy / (bw * zoom);
  setView(z, bx - sx / (bw * z), by - sy / (bw * z), settle);
}
function zoomStep(f) { zoomAt(f, bw / 2, (bw * ASPECT) / 2, true); }
function showZoom() { const b = $('#zoomReset'); if (b) b.textContent = `${Math.round(zoom * 100)}%`; }
// Board point under a pointer event.
function boardPoint(e, rect) {
  return [
    Math.min(1, Math.max(0, panX + (e.clientX - rect.left) / (bw * zoom))),
    Math.min(ASPECT, Math.max(0, panY + (e.clientY - rect.top) / (bw * zoom))),
  ];
}

// Draw only the points added since the last call.
function drawNew(s) {
  const n = s.pts.length / 3;
  if (!hidden.has(s.l)) {
    prep(lctx, s);
    if (s.drawn === 0 && n >= 1) dot(lctx, s);
    for (let i = Math.max(1, s.drawn); i < n; i++) seg(lctx, s, i);
  }
  s.drawn = n;
}

function redrawLive() {
  setCtxView(lctx, curView());
  wipe(lctx);
  for (const s of liveStrokes.values()) { s.drawn = 0; drawNew(s); }
}

// Time-sliced redraw of finished strokes. Invariant: strokes[0..cursor) are on
// the target canvas. `direct` clears the visible canvas and fills it in
// progressively (first load, resize). Otherwise the redraw goes to a spare
// canvas and is copied over when complete, so the board never flashes empty
// (undo, hide, delete on a full board).
let redrawGen = 0, cursor = 0, buffered = false, loopScheduled = false;
let target = { z: 1, x: 0, y: 0 }; // the view the running redraw draws at

function step(gen) {
  if (gen !== redrawGen) return; // a newer redraw replaced this one
  loopScheduled = false;
  const ctx = buffered ? sctx : bctx;
  const t0 = performance.now();
  while (cursor < strokes.length) {
    const s = strokes[cursor++];
    if (!hidden.has(s.l)) drawFull(ctx, s, target);
    if (performance.now() - t0 > REDRAW_BUDGET_MS) break;
  }
  if (cursor < strokes.length) {
    loopScheduled = true;
    requestAnimationFrame(() => step(gen));
  } else if (buffered) {
    buffered = false;
    wipe(bctx);
    bctx.save(); bctx.setTransform(1, 0, 0, 1, 0, 0); bctx.drawImage(spare, 0, 0); bctx.restore();
    rv = target; setCtxView(bctx, rv); stretchBase();
  }
}
function kick() {
  if (loopScheduled) return;
  loopScheduled = true;
  const gen = redrawGen;
  requestAnimationFrame(() => step(gen));
}
function redrawBase(direct) {
  redrawGen++; loopScheduled = false; cursor = 0;
  buffered = !direct;
  target = curView();
  if (direct) { rv = target; setCtxView(bctx, rv); stretchBase(); wipe(bctx); }
  else { setCtxView(sctx, target); wipe(sctx); }
  step(redrawGen);
}

function layout() {
  const vw = innerWidth, vh = innerHeight;
  bw = Math.max(1, Math.floor(Math.min(vw, vh / ASPECT)));
  const bh = Math.floor(bw * ASPECT);
  boardEl.style.cssText = `width:${bw}px;height:${bh}px;left:${(vw - bw) >> 1}px;top:${(vh - bh) >> 1}px`;
  dpr = Math.min(devicePixelRatio || 1, 2); // cap for slow GPUs
  for (const cv of [base, live, spare]) { cv.width = Math.round(bw * dpr); cv.height = Math.round(bh * dpr); }
  redrawBase(true); redrawLive(); placeEditor();
}
addEventListener('resize', layout);
if (window.visualViewport) visualViewport.addEventListener('resize', () => { if (innerWidth && innerHeight) placeEditor(); });

// ---- finished strokes ------------------------------------------------------

const qOf = (s) => (s.q == null ? Infinity : s.q);
const byQ = (a, b) => { const x = qOf(a), y = qOf(b); return x === y ? 0 : x < y ? -1 : 1; };
const bump = (l, d) => counts.set(l, (counts.get(l) || 0) + d);

// Add a finished stroke in q order. `defer` leaves the drawing to the sliced
// loop (snapshots).
function insertStroke(s, defer) {
  if (byId.has(s.id)) return;
  const q = qOf(s);
  let i = strokes.length;
  while (i > 0 && qOf(strokes[i - 1]) > q) i--;
  const tailUnconfirmed = i < strokes.length && strokes[i].q == null;
  strokes.splice(i, 0, s);
  byId.set(s.id, s); bump(s.l, 1);
  if (i >= cursor) {
    // Not reached by the redraw loop yet.
    if (buffered && !hidden.has(s.l)) drawFull(bctx, s, rv); // show it now; the loop adds it to the buffer
    if (!buffered && !defer && i === cursor && i === strokes.length - 1) {
      if (!hidden.has(s.l)) drawFull(bctx, s, rv);
      cursor++;
    } else kick();
  } else if (tailUnconfirmed && !buffered) {
    // Lands just under my own unconfirmed strokes: paint it on top for now
    // rather than redraw the whole board. The array order stays correct.
    if (!hidden.has(s.l)) drawFull(bctx, s, rv);
    cursor++;
  } else {
    redrawBase();
  }
  layersChanged();
}

function removeStroke(id) {
  const s = byId.get(id);
  if (!s) return false;
  byId.delete(id); bump(s.l, -1);
  const i = strokes.lastIndexOf(s);
  if (i >= 0) strokes.splice(i, 1);
  redrawBase();
  layersChanged();
  return true;
}

function dropBoard() {
  strokes.length = 0; byId.clear(); counts.clear(); liveStrokes.clear(); layers.clear();
}

// ---- live strokes (local and remote) ---------------------------------------

function begin(id, l, c, w) {
  const s = { id, l, c, w, pts: [], drawn: 0 };
  liveStrokes.set(id, s);
  return s;
}
function extend(id, pts) {
  const s = liveStrokes.get(id); if (!s || !Array.isArray(pts)) return;
  for (let i = 0; i < pts.length; i++) s.pts.push(pts[i]);
  drawNew(s);
}

// ---- network ---------------------------------------------------------------

let ws = null, retry = 500, stopped = false;
let online = false;  // hello received on the current socket
let ready = false;   // snapshot complete on the current socket

function showStatus(text) {
  if (text) net = text;
  const parts = [net, role, engine];
  if (people > 0) parts.push(`${people} online`);
  statusText.textContent = parts.join(' · ');
}
function send(m) { if (ws && ws.readyState === 1) ws.send(JSON.stringify(m)); }

function connect() {
  if (stopped) return;
  const proto = location.protocol === 'https:' ? 'wss:' : 'ws:';
  const sock = new WebSocket(`${proto}//${location.host}/ws`);
  ws = sock;
  sock.onopen = () => {
    const who = contributorSecret();
    sock.send(JSON.stringify({ t: 'join', key, who }));
  };
  sock.onclose = () => {
    if (ws !== sock) return;
    online = false; ready = false; people = 0;
    if (cur) cur.sent = false;
    statusEl.classList.remove('ok');
    if (stopped) return;
    showStatus('reconnecting');
    setTimeout(connect, retry); retry = Math.min(retry * 2, 8000);
  };
  sock.onmessage = (ev) => {
    if (ws !== sock) return;
    let m;
    try { m = JSON.parse(ev.data); } catch { return; }
    if (m && typeof m === 'object') onMessage(m);
  };
}

function onMessage(m) {
  switch (m.t) {
    case 'hello': onHello(m); break;
    case 'ss':
      if (Array.isArray(m.a)) for (const s of m.a) insertStroke(strokeFrom(s), true);
      break;
    case 'ready': onReady(); break;
    case 'b': begin(m.id, m.l, m.c, m.w); break;
    case 'p': extend(m.id, m.pts); break;
    case 'e':
      if (liveStrokes.delete(m.id)) redrawLive();
      insertStroke(strokeFrom(m));
      break;
    case 'ok': {
      const s = pending.get(m.id); if (!s) break;
      pending.delete(m.id); s.q = m.q;
      const i = strokes.lastIndexOf(s);
      if (i >= 0 && ((i > 0 && qOf(strokes[i - 1]) > s.q) ||
          (i < strokes.length - 1 && qOf(strokes[i + 1]) < s.q))) strokes.sort(byQ);
      break;
    }
    case 'no':
      pending.delete(m.id); removeStroke(m.id);
      toast(m.m ? `Stroke not saved: ${m.m}` : 'Stroke not saved');
      break;
    case 'u':
      pending.delete(m.id);
      if (liveStrokes.delete(m.id)) redrawLive();
      removeStroke(m.id);
      break;
    case 'l':
      if (!layers.has(m.l)) { layers.set(m.l, { l: m.l, c: m.c, del: false }); layersChanged(); }
      break;
    case 'ld': onLayerDeleted(m.l); break;
    case 'lr': onLayerRestored(m.l, m.a); break;
    case 'n': people = m.n | 0; showStatus(); break;
    case 'err': onError(m); break;
  }
}

const strokeFrom = (m) => {
  const s = { id: m.id, l: m.l, q: m.q, c: m.c, w: m.w, pts: m.pts || [] };
  if (typeof m.txt === 'string') s.txt = m.txt;
  return s;
};

function onHello(m) {
  retry = 500;
  role = m.role === 'edit' ? 'edit' : 'view';
  me = role === 'edit' && m.me != null ? m.me : null;
  viewKey = m.view || null;
  online = true; ready = false;
  document.body.classList.toggle('edit', role === 'edit');

  // Drop all board state; the snapshot follows. A stroke I am drawing right
  // now survives: it is sent whole when it ends.
  const keep = cur && role === 'edit' ? liveStrokes.get(cur.id) : null;
  dropBoard();
  if (keep) { keep.l = me; liveStrokes.set(keep.id, keep); cur.sent = false; cur.out = []; }
  else cur = null;
  for (const L of Array.isArray(m.layers) ? m.layers : []) layers.set(L.l, { l: L.l, c: L.c, del: !!L.del });

  const mineNow = me != null && layers.get(me) ? layers.get(me).c : null;
  if (mineNow && (color == null || color === myColor)) color = mineNow;
  myColor = mineNow;
  buildSwatches();

  redrawBase(true); redrawLive();
  statusEl.classList.add('ok'); showStatus('connected');
  fillShare(); layersChanged();
}

function onReady() {
  ready = true;
  if (role !== 'edit' || me == null) { pending.clear(); undoQueue = []; return; }
  // Strokes the relay never confirmed: put them back and send them whole.
  for (const [id, s] of pending) {
    const have = byId.get(id);
    if (have) pending.set(id, have); // the relay stored it; only the `ok` was lost
    else { s.l = me; delete s.q; insertStroke(s); }
    send({ t: 's', id, c: s.c, w: s.w, pts: s.pts, txt: s.txt });
  }
  for (const id of undoQueue) { removeStroke(id); send({ t: 'u', id }); }
  undoQueue = [];
}

function onLayerDeleted(l) {
  let changed = false;
  for (let i = strokes.length - 1; i >= 0; i--) {
    const s = strokes[i];
    if (s.l !== l || pending.has(s.id)) continue; // unconfirmed strokes were not deleted
    strokes.splice(i, 1); byId.delete(s.id); bump(l, -1); changed = true;
  }
  const L = layers.get(l) || { l, c: '#888888', del: true };
  L.del = true; layers.set(l, L);
  if (changed) redrawBase();
  layersChanged();
  const text = `${layerName(l)} layer deleted`;
  if (role === 'edit') toast(text, 'Undo', () => send({ t: 'lr', l }), 8000);
  else toast(text, null, null, 4000);
}

function onLayerRestored(l, a) {
  if (Array.isArray(a)) {
    for (const raw of a) {
      if (byId.has(raw.id)) continue;
      const s = strokeFrom(raw); if (s.l == null) s.l = l;
      strokes.push(s); byId.set(s.id, s); bump(s.l, 1);
    }
    strokes.sort(byQ);
    redrawBase();
  }
  const L = layers.get(l); if (L) L.del = false;
  layersChanged();
}

function onError(m) {
  if (m.code === 'gone') {
    stopped = true;
    showMessage("This board doesn't exist or has expired.", { href: '/', label: 'New board' });
    showStatus('no such board');
  } else if (m.code === 'bad') {
    stopped = true;
    showMessage(m.m || 'This link is not valid.', { href: '/', label: 'New board' });
    showStatus('bad link');
  } else if (m.code === 'busy') {
    retry = 8000; showStatus('board is busy, retrying');
  } else if (m.code === 'rate') {
    retry = Math.max(retry, 2000);
  }
}

// ---- pointer input -----------------------------------------------------------
// Pen: draws. Mouse: left button draws; middle button or Space+drag pans.
// Fingers: two pinch and pan; one draws, until a pen has been seen, after
// which single fingers are ignored (a resting hand). On a view link one
// finger or the mouse pans.

let cur = null;        // { id, pointerId, out:[], sent, pen } stroke being drawn locally
let penSeen = false;   // a pen has been used in this browser
let penAt = -1e9;      // time of the last pen event
let flushQueued = false;
const touches = new Map(); // pointerId -> { x, y } fingers on the board
let pinch = null;      // { d0, z0, bx, by } two-finger zoom in progress
let drag = null;       // { pointerId, x, y } one-pointer pan in progress
let textTap = null;    // { pointerId } a tap that will place text on release
let spaceDown = false;

function addPoint(e, rect) {
  const s = liveStrokes.get(cur.id); if (!s) return;
  const [bx, by] = boardPoint(e, rect);
  const x = r5(bx), y = r5(by);
  const p = e.pointerType === 'pen' && e.pressure > 0 ? Math.round(e.pressure * 1000) / 1000 : 0.5;
  const n = s.pts.length;
  if (n && Math.abs(s.pts[n - 3] - x) + Math.abs(s.pts[n - 2] - y) < 0.0004 / zoom) return; // jitter
  if (n >= 60000) return; // 20,000 points per stroke
  s.pts.push(x, y, p);
  if (cur.sent) cur.out.push(x, y, p);
}

// Draw and send at most one batch of points per animation frame.
function queueFlush() {
  if (flushQueued) return;
  flushQueued = true;
  requestAnimationFrame(() => {
    flushQueued = false;
    if (!cur) return;
    const s = liveStrokes.get(cur.id);
    if (s) drawNew(s);
    if (cur.sent && cur.out.length) { send({ t: 'p', id: cur.id, pts: cur.out }); cur.out = []; }
  });
}

// Throw away the stroke being drawn (a second finger turned it into a pinch).
function abandonStroke() {
  if (!cur) return;
  const id = cur.id, sent = cur.sent;
  cur = null;
  if (liveStrokes.delete(id)) redrawLive();
  if (sent) send({ t: 'u', id });
}

function capture(e) { try { live.setPointerCapture(e.pointerId); } catch { /* pointer already gone */ } }

function startPinch() {
  if (cur && !cur.pen) abandonStroke();
  drag = null; textTap = null;
  const [a, b] = [...touches.values()];
  const rect = boardEl.getBoundingClientRect();
  const mx = (a.x + b.x) / 2 - rect.left, my = (a.y + b.y) / 2 - rect.top;
  pinch = { d0: Math.hypot(a.x - b.x, a.y - b.y) || 1, z0: zoom,
    bx: panX + mx / (bw * zoom), by: panY + my / (bw * zoom) };
}

live.addEventListener('pointerdown', (e) => {
  const t = e.pointerType, now = performance.now();
  if (t === 'pen') { penSeen = true; penAt = now; }
  if (t === 'touch') {
    // A hand landing while the pen is at work is not a gesture.
    if ((cur && cur.pen) || now - penAt < 400) return;
    touches.set(e.pointerId, { x: e.clientX, y: e.clientY });
    if (touches.size === 2) { e.preventDefault(); startPinch(); return; }
    if (touches.size > 2) return;
  }
  if (pinch) return;
  e.preventDefault();
  if (editor) { closeEditor(true); return; } // a tap outside the text box finishes it
  const viewer = role !== 'edit' || me == null;
  if ((t === 'mouse' && (e.button === 1 || (e.button === 0 && spaceDown))) ||
      (viewer && (t !== 'mouse' || e.button === 0))) {
    capture(e); drag = { pointerId: e.pointerId, x: e.clientX, y: e.clientY };
    return;
  }
  if (t === 'mouse' && e.button !== 0) return;
  if (viewer || cur || !color) return;
  if (t === 'touch' && penSeen) return;
  capture(e);
  if (tool === 'text') { textTap = { pointerId: e.pointerId }; return; }
  // Button 5 / buttons bit 32 is the eraser end of a pen.
  const penEraser = t === 'pen' && (e.button === 5 || (e.buttons & 32) !== 0);
  const erase = tool === 'erase' || penEraser;
  const id = Math.random().toString(36).slice(2, 12) + Date.now().toString(36);
  const s = begin(id, me, erase ? ERASER_COLOR : color, erase ? width * ERASER_SCALE : width);
  cur = { id, pointerId: e.pointerId, out: [], sent: online && ready, pen: t === 'pen' };
  if (cur.sent) send({ t: 'b', id, c: s.c, w: s.w });
  addPoint(e, boardEl.getBoundingClientRect());
  queueFlush();
});

live.addEventListener('pointermove', (e) => {
  if (e.pointerType === 'pen') penAt = performance.now();
  const f = e.pointerType === 'touch' ? touches.get(e.pointerId) : null;
  if (f) {
    f.x = e.clientX; f.y = e.clientY;
    if (pinch && touches.size >= 2) {
      const [a, b] = [...touches.values()];
      const rect = boardEl.getBoundingClientRect();
      const z = Math.min(MAX_ZOOM, Math.max(1, pinch.z0 * Math.hypot(a.x - b.x, a.y - b.y) / pinch.d0));
      const mx = (a.x + b.x) / 2 - rect.left, my = (a.y + b.y) / 2 - rect.top;
      setView(z, pinch.bx - mx / (bw * z), pinch.by - my / (bw * z));
      return;
    }
  }
  if (drag && e.pointerId === drag.pointerId) {
    setView(zoom, panX - (e.clientX - drag.x) / (bw * zoom), panY - (e.clientY - drag.y) / (bw * zoom));
    drag.x = e.clientX; drag.y = e.clientY;
    return;
  }
  if (!cur || e.pointerId !== cur.pointerId) return;
  const rect = boardEl.getBoundingClientRect();
  // Coalesced events give every pen sample, not just one per frame.
  const evs = e.getCoalescedEvents ? e.getCoalescedEvents() : [];
  if (evs.length) for (const c of evs) addPoint(c, rect); else addPoint(e, rect);
  queueFlush();
});

function endStroke(e) {
  if (!cur || e.pointerId !== cur.pointerId) return;
  const id = cur.id, sent = cur.sent, out = cur.out, s = liveStrokes.get(id);
  cur = null;
  if (!s) return;
  if (sent && out.length) send({ t: 'p', id, pts: out });
  liveStrokes.delete(id);
  if (!s.pts.length) { redrawLive(); if (sent) send({ t: 'u', id }); return; }
  // Zoomed in, detail is finer: tighten the tolerance to match.
  const f = { id, l: me, c: s.c, w: s.w, pts: simplify(s.pts, EPS / zoom) };
  insertStroke(f); redrawLive();
  pending.set(id, f); mine.push(id);
  // Confirmed by `ok`. If the socket is down or still loading the snapshot,
  // the stroke goes out as `s` when `ready` arrives.
  if (sent) send({ t: 'e', id, c: f.c, w: f.w, pts: f.pts });
  else if (online && ready) send({ t: 's', id, c: f.c, w: f.w, pts: f.pts });
}

function pointerEnd(e) {
  if (e.pointerType === 'pen') penAt = performance.now();
  if (touches.delete(e.pointerId) && pinch && touches.size < 2) { pinch = null; settleView(); }
  if (drag && drag.pointerId === e.pointerId) { drag = null; settleView(); return; }
  if (textTap && textTap.pointerId === e.pointerId) {
    textTap = null;
    if (e.type === 'pointerup') textAt(e);
    return;
  }
  endStroke(e);
}
live.addEventListener('pointerup', pointerEnd);
live.addEventListener('pointercancel', pointerEnd);
live.addEventListener('contextmenu', (e) => e.preventDefault());

// Trackpad and wheel: pinch (or Ctrl/Cmd + wheel) zooms, scrolling pans.
addEventListener('wheel', (e) => {
  if (e.target.closest && e.target.closest('.panel')) return;
  e.preventDefault();
  const rect = boardEl.getBoundingClientRect(), k = e.deltaMode === 1 ? 16 : 1;
  if (e.ctrlKey || e.metaKey) {
    // A trackpad pinch sends small steps; a mouse wheel notch sends 100 or more. Cap a step at about 1.35x.
    const d = Math.max(-30, Math.min(30, e.deltaY * k));
    zoomAt(Math.exp(-d * 0.01), e.clientX - rect.left, e.clientY - rect.top);
  }
  else setView(zoom, panX + (e.deltaX * k) / (bw * zoom), panY + (e.deltaY * k) / (bw * zoom));
}, { passive: false });

// Safari: stop the page itself zooming; on a Mac trackpad use the gesture.
let gestureZoom = 1;
addEventListener('gesturestart', (e) => { e.preventDefault(); gestureZoom = zoom; });
addEventListener('gesturechange', (e) => {
  e.preventDefault();
  if (touches.size || !e.scale) return; // fingers on the board are handled as pointers
  const rect = boardEl.getBoundingClientRect();
  zoomAt((gestureZoom * e.scale) / zoom, e.clientX - rect.left, e.clientY - rect.top);
});
addEventListener('gestureend', (e) => { e.preventDefault(); });

// ---- text annotations --------------------------------------------------------
// A text annotation is an item in my layer like a stroke: one anchor point
// (its top left), a font size in board units, and the text.

const ta = $('#textEdit');
let editor = null; // { x, y, c, w, old } while the text box is open

function placeEditor() {
  if (!editor) return;
  const px = editor.w * bw * zoom;
  const left = (editor.x - panX) * bw * zoom, top = (editor.y - panY) * bw * zoom;
  ta.style.left = `${left}px`; ta.style.top = `${top}px`;
  ta.style.fontSize = `${px}px`; ta.style.color = editor.c;
  ta.style.width = `${Math.max(60, bw - left - 2)}px`;
  ta.style.height = 'auto';
  ta.style.height = `${Math.max(ta.scrollHeight, px * LINE_HEIGHT)}px`;
}

function openEditor(x, y, old) {
  editor = { x, y, c: old ? old.c : color, w: old ? old.w : width * TEXT_SCALE, old: old || null };
  if (old) removeStroke(old.id); // shown in the box while it is edited
  ta.value = old ? old.txt : '';
  ta.hidden = false;
  placeEditor();
  ta.focus();
}

function closeEditor(save) {
  if (!editor) return;
  const ed = editor; editor = null;
  const txt = ta.value.replace(/\r/g, '').split('\n').slice(0, 20).join('\n').replace(/\s+$/, '');
  ta.hidden = true; ta.value = ''; ta.blur();
  const old = ed.old;
  if (old && (!save || txt === old.txt)) { insertStroke(old); return; } // unchanged: put it back
  if (old) { // replaced or emptied: the old one goes, as an undo
    pending.delete(old.id);
    if (online && ready) send({ t: 'u', id: old.id }); else undoQueue.push(old.id);
  }
  if (!save || !txt.trim() || me == null) return;
  const id = Math.random().toString(36).slice(2, 12) + Date.now().toString(36);
  const f = { id, l: me, c: ed.c, w: r5(ed.w), pts: [r5(ed.x), r5(ed.y), 0.5], txt };
  insertStroke(f); pending.set(id, f); mine.push(id);
  if (online && ready) send({ t: 's', id, c: f.c, w: f.w, pts: f.pts, txt });
}

// A tap with the text tool: edit my text under it, or start a new one there.
function textAt(e) {
  const [x, y] = boardPoint(e, boardEl.getBoundingClientRect());
  for (let i = strokes.length - 1; i >= 0; i--) {
    const s = strokes[i];
    if (s.txt == null || s.l !== me || hidden.has(s.l)) continue;
    const b = bbox(s);
    if (x >= b[0] && x <= b[2] && y >= b[1] && y <= b[3]) return openEditor(s.pts[0], s.pts[1], s);
  }
  const h = width * TEXT_SCALE * LINE_HEIGHT;
  openEditor(x, Math.max(0, Math.min(ASPECT - h, y - h / 2)));
}

ta.addEventListener('input', placeEditor);
ta.addEventListener('blur', () => closeEditor(true));
ta.addEventListener('keydown', (e) => {
  e.stopPropagation();
  if (e.key === 'Enter' && !e.shiftKey && !e.isComposing) { e.preventDefault(); closeEditor(true); }
  else if (e.key === 'Escape') { e.preventDefault(); closeEditor(false); }
});

function undo() {
  if (role !== 'edit') return;
  while (mine.length) {
    const id = mine.pop();
    if (!byId.has(id)) continue; // already gone (layer deleted, refused)
    removeStroke(id);
    pending.delete(id);
    if (online && ready) send({ t: 'u', id }); else undoQueue.push(id);
    return;
  }
}
addEventListener('keydown', (e) => {
  if ((e.ctrlKey || e.metaKey) && !e.shiftKey && !e.altKey && e.key.toLowerCase() === 'z') {
    if (e.target instanceof HTMLInputElement) return;
    e.preventDefault(); undo();
  } else if (e.key === 'Escape') openPanel(null);
  else if (e.target instanceof HTMLInputElement || e.ctrlKey || e.metaKey || e.altKey) return;
  else if (e.key === '+' || e.key === '=') zoomStep(1.5);
  else if (e.key === '-') zoomStep(1 / 1.5);
  else if (e.key === '0') setView(1, 0, 0, true);
  else if (e.key === ' ') { spaceDown = true; e.preventDefault(); }
});
addEventListener('keyup', (e) => { if (e.key === ' ') spaceDown = false; });
addEventListener('blur', () => { spaceDown = false; });

// ---- toolbar ---------------------------------------------------------------

const swatchesEl = $('#swatches'), eraserBtn = $('#eraser'), textBtn = $('#textBtn');

function buildSwatches() {
  swatchesEl.replaceChildren();
  if (!myColor) return;
  const list = [myColor, ...FIXED_COLORS.filter((c) => c.toLowerCase() !== myColor.toLowerCase())];
  for (const c of list) {
    const b = el('button', 'sw');
    b.type = 'button'; b.dataset.c = c;
    b.style.background = c;
    b.setAttribute('aria-label', c === myColor ? 'My colour' : COLOR_NAMES[c] || c);
    swatchesEl.append(b);
  }
  if (!list.includes(color)) color = myColor;
  markTools();
}
function markTools() {
  for (const b of document.querySelectorAll('#bar [data-c]')) {
    const on = tool !== 'erase' && b.dataset.c === color;
    b.classList.toggle('on', on); b.setAttribute('aria-pressed', String(on));
  }
  eraserBtn.classList.toggle('on', tool === 'erase'); eraserBtn.setAttribute('aria-pressed', String(tool === 'erase'));
  textBtn.classList.toggle('on', tool === 'text'); textBtn.setAttribute('aria-pressed', String(tool === 'text'));
  document.body.classList.toggle('text', tool === 'text');
  for (const b of document.querySelectorAll('#bar [data-w]')) {
    const on = Number(b.dataset.w) === width;
    b.classList.toggle('on', on); b.setAttribute('aria-pressed', String(on));
  }
}

$('#bar').addEventListener('click', (e) => {
  const b = e.target.closest('button'); if (!b) return;
  if (b.dataset.c) { color = b.dataset.c; if (tool === 'erase') tool = 'pen'; markTools(); }
  else if (b.dataset.w) { width = Number(b.dataset.w); markTools(); }
  else if (b.id === 'eraser') { tool = tool === 'erase' ? 'pen' : 'erase'; markTools(); }
  else if (b.id === 'textBtn') { tool = tool === 'text' ? 'pen' : 'text'; markTools(); }
  else if (b.id === 'zoomIn') zoomStep(1.5);
  else if (b.id === 'zoomOut') zoomStep(1 / 1.5);
  else if (b.id === 'zoomReset') setView(1, 0, 0, true);
  else if (b.id === 'undo') undo();
  else if (b.id === 'layersBtn') openPanel(layersPanel.hidden ? layersPanel : null);
  else if (b.id === 'shareBtn') openPanel(sharePanel.hidden ? sharePanel : null);
});

// ---- panels ----------------------------------------------------------------

function openPanel(p) {
  layersPanel.hidden = p !== layersPanel;
  sharePanel.hidden = p !== sharePanel;
  $('#layersBtn').setAttribute('aria-expanded', String(p === layersPanel));
  $('#shareBtn').setAttribute('aria-expanded', String(p === sharePanel));
  if (p === layersPanel) renderLayers();
  if (p === sharePanel) fillShare();
}
for (const b of document.querySelectorAll('[data-close]')) b.addEventListener('click', () => openPanel(null));

const layerName = (l) => (l === me ? 'Your' : `Guest ${l}`);

let layersQueued = false;
function layersChanged() {
  if (layersPanel.hidden || layersQueued) return;
  layersQueued = true;
  requestAnimationFrame(() => { layersQueued = false; renderLayers(); });
}

function renderLayers() {
  if (layersPanel.hidden) return;
  const rows = [];
  const sorted = [...layers.values()].sort((a, b) => (a.l === me ? -1 : b.l === me ? 1 : a.l - b.l));
  for (const L of sorted) {
    const n = counts.get(L.l) || 0, off = hidden.has(L.l);
    const li = el('li');
    if (n === 0 && !L.del && L.l !== me) li.classList.add('dim');
    if (off) li.classList.add('off');
    const d = el('span', 'dot'); d.style.background = L.c;
    const who = el('span', 'who', L.l === me ? 'You' : `Guest ${L.l}`);
    who.append(el('small', null, `${n} item${n === 1 ? '' : 's'}${L.del ? ' · deleted' : ''}`));
    li.append(d, who);
    li.append(layerButton(off ? 'Show' : 'Hide', 'eye', L.l,
      `${off ? 'Show' : 'Hide'} ${layerName(L.l)} layer (only for me)`));
    if (role === 'edit') {
      if (L.del) li.append(layerButton('Restore', 'lr', L.l, `Restore ${layerName(L.l)} layer for everyone`));
      if (n > 0) {
        const del = layerButton('Delete', 'ld', L.l, `Delete ${layerName(L.l)} layer for everyone`);
        del.classList.add('danger'); li.append(del);
      }
    }
    rows.push(li);
  }
  if (!rows.length) rows.push(el('li', 'dim', 'No layers yet.'));
  layerList.replaceChildren(...rows);
}
function layerButton(text, act, l, label) {
  const b = el('button', null, text);
  b.type = 'button'; b.dataset.act = act; b.dataset.l = String(l);
  b.setAttribute('aria-label', label);
  return b;
}

layerList.addEventListener('click', (e) => {
  const b = e.target.closest('button'); if (!b) return;
  const l = Number(b.dataset.l);
  if (b.dataset.act === 'eye') {
    if (!hidden.delete(l)) hidden.add(l);
    saveHidden(); redrawBase(); redrawLive(); renderLayers();
  } else if (role === 'edit' && (b.dataset.act === 'ld' || b.dataset.act === 'lr')) {
    if (online && ready) send({ t: b.dataset.act, l });
    else toast('Not connected. Try again in a moment.');
  }
});

function fillShare() {
  const at = (k) => `${location.origin}/b/${k}`;
  const edit = role === 'edit';
  $('#editRow').hidden = !edit;
  $('#editLink').value = edit ? at(key) : '';
  $('#viewLink').value = edit ? (viewKey ? at(viewKey) : '') : at(key);
}
sharePanel.addEventListener('click', (e) => {
  const b = e.target.closest('button[data-copy]'); if (!b) return;
  const input = document.getElementById(b.dataset.copy);
  if (!input.value) return;
  const done = () => { b.textContent = 'Copied'; setTimeout(() => { b.textContent = 'Copy'; }, 1500); };
  const fallback = () => {
    input.focus(); input.select();
    try { if (document.execCommand('copy')) done(); } catch { /* leave it selected */ }
  };
  if (navigator.clipboard && navigator.clipboard.writeText) {
    navigator.clipboard.writeText(input.value).then(done, fallback);
  } else fallback();
});

// ---- toast and message card ------------------------------------------------

let toastTimer = 0;
function toast(text, actionLabel, action, ms) {
  clearTimeout(toastTimer);
  toastEl.replaceChildren(el('span', null, text));
  toastEl.classList.toggle('plain', !actionLabel);
  if (actionLabel) {
    const b = el('button', null, actionLabel); b.type = 'button';
    b.addEventListener('click', () => { toastEl.hidden = true; clearTimeout(toastTimer); action(); });
    toastEl.append(b);
  }
  toastEl.hidden = false;
  toastTimer = setTimeout(() => { toastEl.hidden = true; }, ms || 4000);
}

// opts: { href, label } for a link, or { label, onClick } for a button.
function showMessage(text, opts) {
  msgEl.replaceChildren(el('p', null, text));
  if (opts && opts.href) {
    const a = el('a', null, opts.label); a.href = opts.href; msgEl.append(a);
  } else if (opts && opts.onClick) {
    const b = el('button', null, opts.label); b.type = 'button';
    b.addEventListener('click', opts.onClick); msgEl.append(b);
  }
  msgEl.hidden = false;
}

// ---- start -----------------------------------------------------------------

async function createBoard() {
  msgEl.hidden = true;
  showStatus('creating board');
  const retryOpt = { label: 'Retry', onClick: createBoard };
  let res;
  try { res = await fetch('/api/boards', { method: 'POST' }); }
  catch { showStatus('offline'); return showMessage("Couldn't reach the server to make a board.", retryOpt); }
  if (res.status === 429) {
    showStatus('waiting');
    return showMessage('Too many new boards from this address. Wait a few seconds and try again.', retryOpt);
  }
  let body = null;
  try { body = await res.json(); } catch { /* handled below */ }
  if (!res.ok || !body || typeof body.edit !== 'string') {
    showStatus('error');
    return showMessage("The server couldn't make a board.", retryOpt);
  }
  history.replaceState(null, '', '/b/' + body.edit);
  start(body.edit);
}

function start(k) {
  key = k;
  loadHidden();
  showStatus('connecting');
  connect();
}

function boot() {
  layout();
  const path = location.pathname;
  const m = /^\/b\/([A-Za-z0-9_-]{1,64})\/?$/.exec(path);
  if (m) start(m[1]);
  else if (path === '/' || path === '/index.html') createBoard();
  else showMessage("This board doesn't exist or has expired.", { href: '/', label: 'New board' });
}
boot();
})();
