// the Rust page in headless Chromium against a local relay.
import { createRequire } from 'module';
const { chromium } = createRequire(import.meta.url)((process.env.NODE_PATH ? process.env.NODE_PATH + '/' : '') + 'playwright-core');
const BASE = process.argv[2] || 'http://127.0.0.1:8799';
const errs = [];
const res = [];
const ok = (name, cond, extra = '') => { res.push(`${cond ? 'PASS' : 'FAIL'} ${name}${extra ? ' ' + extra : ''}`); };
const sleep = (ms) => new Promise(r => setTimeout(r, ms));

function watch(page, tag) {
  page.on('console', m => { if (m.type() === 'error' || m.type() === 'warning') errs.push(`${tag} console.${m.type()}: ${m.text()}`); });
  page.on('pageerror', e => errs.push(`${tag} pageerror: ${e.message}`));
}
const ink = (page, only) => page.evaluate((only) => {
  let n = 0;
  for (const id of (only ? [only] : ['base', 'live'])) {
    const c = document.getElementById(id); const x = c.getContext('2d');
    const d = x.getImageData(0, 0, c.width, c.height).data;
    for (let i = 0; i < d.length; i += 4) if (d[i + 3] > 0 && (d[i] < 200 || d[i + 1] < 200 || d[i + 2] < 200)) n++;
  }
  return n;
}, only);
async function stroke(page, x0, y0, dx, dy) {
  await page.mouse.move(x0, y0); await page.mouse.down();
  for (let i = 1; i <= 12; i++) await page.mouse.move(x0 + dx * i / 12, y0 + dy * i / 12);
  await page.mouse.up();
}

const browser = await chromium.launch();
try {
  const ctx = await browser.newContext({ viewport: { width: 1200, height: 800 } });
  const p = await ctx.newPage(); watch(p, 'A');
  await p.goto(BASE + '/');
  await p.waitForFunction(() => document.body.classList.contains('local'), null, { timeout: 8000 }).catch(() => {});
  ok('private board boots (body.local + edit)', await p.evaluate(() => document.body.classList.contains('local') && document.body.classList.contains('edit')));
  ok('needs-wasm card stays hidden', await p.evaluate(() => document.getElementById('needsWasm').hidden));
  ok('status text set', (await p.textContent('#status span')).trim().length > 0, JSON.stringify(await p.textContent('#status')));
  const before = await ink(p);
  await stroke(p, 300, 300, 300, 120);
  await sleep(400);
  const after = await ink(p);
  ok('pen stroke draws ink', after > before + 50, `${before}->${after}`);
  await sleep(1800);
  const saved = await p.evaluate(() => { for (let i = 0; i < localStorage.length; i++) { const k = localStorage.key(i); const v = localStorage.getItem(k); if (/"a":\[\{/.test(v || '')) return k; } return null; });
  ok('private board saved to localStorage', !!saved, String(saved));
  await p.reload();
  await sleep(1200);
  ok('stroke survives reload', (await ink(p)) > 50, String(await ink(p)));

  // second tab on the private board: read-only (F6)
  const p2 = await ctx.newPage(); watch(p2, 'A2');
  await p2.goto(BASE + '/'); await sleep(1200);
  const t2 = await p2.evaluate(() => ({ edit: document.body.classList.contains('edit'), toast: document.getElementById('toast').hidden ? '' : document.getElementById('toast').textContent }));
  ok('second tab is read-only (F6)', !t2.edit, JSON.stringify(t2));
  await p2.close(); await sleep(800);

  // text tool + Enter (W1)
  await p.click('#textBtn');
  await p.mouse.click(400, 550);
  await sleep(200);
  const edOpen = await p.evaluate(() => getComputedStyle(document.getElementById('textEdit')).display !== 'none' && !document.getElementById('textEdit').hidden);
  ok('text editor opens', edOpen);
  await p.keyboard.type('hello');
  const inkT = await ink(p);
  await p.keyboard.press('Enter');
  await sleep(400);
  ok('Enter commits text without a borrow panic', !errs.some(e => /already borrowed|BorrowMut|RuntimeError|unreachable/.test(e)) && (await ink(p)) > inkT - 5);
  await p.click('#penBtn');

  // share
  await p.click('#shareBtn'); await sleep(300);
  await p.click('#shareNow').catch(e => errs.push('shareNow click: ' + e.message));
  await p.waitForURL(/\/b\//, { timeout: 8000 }).catch(() => {});
  const url = p.url();
  ok('share moves to /b/<key>', /\/b\/[A-Za-z0-9_-]{8,}/.test(url), url.replace(/\/b\/.*/, '/b/<key>'));
  await p.waitForFunction(() => document.getElementById('status').classList.contains('ok'), null, { timeout: 8000 }).catch(() => {});
  ok('connected after share (status.ok)', await p.evaluate(() => document.getElementById('status').classList.contains('ok')));
  await sleep(1500);
  const viewLink = await p.evaluate(() => document.getElementById('viewLink')?.value || document.getElementById('viewLink')?.textContent || '');
  const editLink = await p.evaluate(() => document.getElementById('editLink')?.value || document.getElementById('editLink')?.textContent || '');
  const editKey = url.split('/b/')[1];
  ok('view link is not the edit key (F10)', viewLink.length > 0 && !viewLink.includes(editKey), viewLink ? 'set' : 'empty');
  ok('edit link carries the edit key', editLink.includes(editKey));

  // second browser context joins the shared board and sees the ink
  const ctx2 = await browser.newContext({ viewport: { width: 1200, height: 800 } });
  const q = await ctx2.newPage(); watch(q, 'B');
  await q.goto(url);
  await q.waitForFunction(() => document.getElementById('status').classList.contains('ok'), null, { timeout: 8000 }).catch(() => {});
  await sleep(1500);
  ok('second person sees the shared strokes', (await ink(q)) > 50, String(await ink(q)));
  const qb = await ink(q);
  await stroke(p, 250, 650, 400, -60);
  await sleep(1500);
  ok('live stroke reaches the other person', (await ink(q)) > qb + 50, `${qb}->${await ink(q)}`);
  const who = await q.evaluate(() => document.getElementById('whoText').textContent);
  ok('people count shown', /\d|you/i.test(who), JSON.stringify(who));

  // view-only link opens read-only
  if (viewLink) {
    const v = await ctx2.newPage(); watch(v, 'V');
    await v.goto(viewLink.startsWith('http') ? viewLink : BASE + viewLink); await sleep(2000);
    ok('view link opens read-only with ink', !(await v.evaluate(() => document.body.classList.contains('edit'))) && (await ink(v)) > 50);
    await v.close();
  }

  // crumple -> shelf 1, then uncrumple from the shelf
  await p.click('#crumpleBtn'); await sleep(2500);
  const shelfN = await p.textContent('#shelfN');
  ok('crumple puts a sheet on the shelf', shelfN.trim() === '1', JSON.stringify(shelfN));
  ok('board is blank after crumple', (await ink(p)) < 50, String(await ink(p)));
  await p.click('#shelfBtn'); await sleep(400);
  const items = await p.$$('#shelfList button, #shelfList li');
  ok('shelf lists the sheet', items.length > 0, String(items.length));
  const btn = await p.$('#shelfList button') || items[0];
  if (btn) await btn.click();
  const trace = [];
  for (let i = 0; i < 24; i++) { await sleep(250); trace.push(await ink(p)); }
  ok('uncrumple brings the ink back', trace[trace.length - 1] > 50, trace.join(','));

  // ping
  await p.click('#pingBtn');
  await p.mouse.move(500, 300); await p.mouse.down();
  for (let i = 0; i <= 24; i++) { const a = i / 24 * Math.PI * 2; await p.mouse.move(500 + 80 * Math.cos(a), 300 + 80 * Math.sin(a)); }
  await p.mouse.up(); await sleep(300);
  ok('ping sends without error', !errs.some(e => /pageerror/.test(e)));

  // zoom
  const z0 = await p.textContent('#zoomReset');
  await p.click('#zoomIn'); await sleep(300);
  ok('zoom in changes the zoom label', (await p.textContent('#zoomReset')) !== z0, `${z0}->${await p.textContent('#zoomReset')}`);
  await p.click('#zoomReset');
  const zt = []; for (let i = 0; i < 16; i++) { await sleep(250); zt.push((await ink(p, 'base')) + '/' + (await ink(p, 'live'))); }
  res.push('INFO after zoom reset: ' + zt.join(',') + ' label ' + await p.textContent('#zoomReset'));

  // name change (the people panel itself is not opened: an editable text input crashes this
  // headless Chromium build on its own, with or without the page)
  await p.evaluate(() => { const i = document.getElementById('nameIn'); i.value = 'Smoke Tester'; i.dispatchEvent(new Event('change')); });
  await sleep(300);
  ok('name change is kept', (await p.evaluate(() => localStorage.getItem('wb.name'))) === 'Smoke Tester' && /Smoke Tester/.test(await p.textContent('#whoText')));

  // undo
  await sleep(1500);
  const iu = await ink(p);
  await stroke(p, 200, 200, 100, 100); await sleep(1500);
  const iu2 = await ink(p);
  await p.click('#undo'); await sleep(1500);
  const iu3 = await ink(p);
  ok('undo removes the last stroke', iu3 < iu2 - 20 && Math.abs(iu3 - iu) < 30, `${iu}->${iu2}->${iu3}`);

  await ctx2.close(); await ctx.close();

  // F1: a damaged private board is kept and offered for download
  const c3 = await browser.newContext(); const d = await c3.newPage(); watch(d, 'D');
  await d.goto(BASE + '/'); await sleep(800);
  await d.evaluate(() => localStorage.setItem('wb.local', '{"v":1,"a":[{"broken'));
  await d.reload(); await sleep(1200);
  const m = await d.evaluate(() => { const e = document.getElementById('msg'); return { shown: !e.hidden, text: e.textContent, dl: !!e.querySelector('a[download]') }; });
  ok('damaged board: message + download offered (F1)', m.shown && /could not be read/.test(m.text) && m.dl, JSON.stringify(m.text));
  ok('damaged board: stored text untouched', (await d.evaluate(() => localStorage.getItem('wb.local'))) === '{"v":1,"a":[{"broken');
  await c3.close();

  // needs-wasm card when the module cannot load
  const c4 = await browser.newContext(); const w = await c4.newPage();
  await w.route('**/page_bg.wasm', r => r.abort());
  await w.goto(BASE + '/'); await sleep(1000);
  ok('needs-wasm card shows when wasm fails', await w.evaluate(() => !document.getElementById('needsWasm').hidden));
  await c4.close();
} catch (e) {
  errs.push('harness: ' + e.stack);
} finally {
  await browser.close();
}
console.log(res.join('\n'));
// Browser noise, not the page: the readback warning comes from ink() above, the rest from
// headless Chromium's software WebGL.
const noise = /favicon|willReadFrequently|fallback to software WebGL|GPU stall due to ReadPixels/;
const real = errs.filter(e => !noise.test(e));
console.log(`console/page errors: ${real.length}`);
for (const e of real.slice(0, 20)) console.log('  ' + e.slice(0, 300));
process.exit(res.some(r => r.startsWith('FAIL')) || real.length ? 1 : 0);
