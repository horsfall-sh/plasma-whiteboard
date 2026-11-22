// Random cases for the geom/view parity test: strokes, a pen width and a run of view actions.
// Seeded, so every run checks the same cases.
let a = 142;
const rnd = () => { a = (a + 0x6d2b79f5) | 0; let t = Math.imul(a ^ (a >>> 15), 1 | a); t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t; return ((t ^ (t >>> 14)) >>> 0) / 4294967296; };
const choice = (xs) => xs[Math.floor(rnd() * xs.length)];
const uni = (lo, hi) => lo + (hi - lo) * rnd();
const r = (v, d) => Math.round(v * 10 ** d) / 10 ** d;
const cs = [];
for (let t = 0; t < 400; t++) {
  const n = choice([0, 1, 2, 3, 4, 5, 9, 20, 60, 200]); let x = 0, y = 0; const pts = [];
  for (let i = 0; i < n; i++) {
    if (!(rnd() < 0.1 && pts.length)) { x += uni(-0.03, 0.03); y += uni(-0.03, 0.03); }
    pts.push(r(x, 5), r(y, 5), choice([0.5, r(rnd(), 3)]));
  }
  const acts = [];
  for (let i = 0; i < 8; i++) {
    const k = choice(['za', 'home', 'ps', 'set', 'rs']);
    if (k === 'za') acts.push([k, choice([0.5, 0.8, 1.25, 2, 3]), uni(0, 1600), uni(0, 900)]);
    else if (k === 'ps') acts.push([k, ...choice([[1, 0], [-1, 0], [0, 1], [0, -1]])]);
    else if (k === 'set') acts.push([k, uni(0.1, 9), uni(-2, 2), uni(-1, 1)]);
    else if (k === 'rs') acts.push([k, choice([400, 800, 1024, 1600, 2560]), choice([600, 800, 900, 1366])]);
    else acts.push([k]);
  }
  cs.push({ pts, px: choice([300, 1000, 4000]), w: choice([0.002, 0.004, 0.01]), bw: choice([500, 1000, 1481.4814814814813]),
    hue: Math.floor(rnd() * 360), vw: choice([400, 1080, 1600]), vh: choice([800, 600, 900]), acts });
}
console.log(JSON.stringify(cs));
