// a panic in the page shows the "something broke" card (Firefox and Chromium).
import { createRequire } from 'module';
const pw = createRequire(import.meta.url)((process.env.NODE_PATH ? process.env.NODE_PATH + '/' : '') + 'playwright-core');
const BASE = process.argv[2] || 'http://127.0.0.1:8799';
const res = [], errs = [];
const ok = (n, c, x = '') => res.push(`${c ? 'PASS' : 'FAIL'} ${n}${x ? ' ' + x : ''}`);
const sleep = ms => new Promise(r => setTimeout(r, ms));
for (const name of ['firefox', 'chromium']) {
  const b = await pw[name].launch();
  try {
    const ctx = await b.newContext({ viewport: { width: 1200, height: 800 }, bypassCSP: true });
    const p = await ctx.newPage();
    const logs = [];
    p.on('console', m => { if (m.type() === 'error') logs.push(m.text()); });
    p.on('pageerror', e => logs.push('pageerror: ' + e.message));
    await p.goto(BASE + '/'); await sleep(1500);
    await p.mouse.move(300, 300); await p.mouse.down();
    for (let i = 1; i <= 12; i++) await p.mouse.move(300 + 25 * i, 300 + 10 * i);
    await p.mouse.up(); await sleep(1500);
    const shown = () => p.evaluate(() => !document.getElementById('broke').hidden);
    ok(`${name}: no card while the page works`, !(await shown()));
    // force a panic at a real call site: canvas() expects createElement to succeed
    await p.evaluate(() => { const ce = Document.prototype.createElement; Document.prototype.createElement = function (t, ...r) { if (String(t).toLowerCase() === 'canvas') throw new Error('forced by the G6 test'); return ce.call(this, t, ...r); }; });
    await p.click('#crumpleBtn'); await sleep(2500);
    if (!(await shown())) { await p.click('#shelfBtn').catch(() => {}); await sleep(800); }
    ok(`${name}: a panic shows the card`, await shown(), JSON.stringify(await p.evaluate(() => document.getElementById('broke').textContent)));
    ok(`${name}: the panic is logged`, logs.some(l => /the page stopped/.test(l)), JSON.stringify(logs.slice(0, 3)).slice(0, 300));
    const r = await p.evaluate(() => { const a = document.querySelector('#broke a').getBoundingClientRect(); const st = getComputedStyle(document.querySelector('#broke a')); return [Math.round(a.width), Math.round(a.height), st.display, st.lineHeight, document.querySelector('#broke a').getAttribute('href')]; });
    // the headless Chromium shell renders no glyphs here (no fonts), so its box has no height
    ok(`${name}: the Reload link is there`, r[2] === 'inline-block' && r[3] === '44px' && r[4] === '' && (name !== 'firefox' || r[1] >= 40), JSON.stringify(r));
  } catch (e) { errs.push(name + ' harness: ' + e.stack); } finally { await b.close(); }
}
console.log(res.join('\n')); console.log('errors: ' + errs.length); for (const e of errs) console.log('  ' + e.slice(0, 400));
process.exit(res.some(r => r.startsWith('FAIL')) || errs.length ? 1 : 0);
