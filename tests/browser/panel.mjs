// the people panel typed into for real, in headless Firefox (Chromium's shell crashes on text inputs).
// bypassCSP is only for Playwright's own injected eval under Firefox; the page itself needs no eval.
import { createRequire } from 'module';
const { firefox } = createRequire(import.meta.url)((process.env.NODE_PATH ? process.env.NODE_PATH + '/' : '') + 'playwright-core');
const BASE = process.argv[2] || 'http://127.0.0.1:8799';
const errs = [], res = [];
const ok = (n, c, x = '') => res.push(`${c ? 'PASS' : 'FAIL'} ${n}${x ? ' ' + x : ''}`);
const sleep = ms => new Promise(r => setTimeout(r, ms));
const watch = (p, t) => { p.on('console', m => { if (m.type() === 'error') errs.push(`${t} console.error: ${m.text()}`); }); p.on('pageerror', e => errs.push(`${t} pageerror: ${e.message}`)); };
const ls = (p, k) => p.evaluate(k => localStorage.getItem(k), k);
const opts = { viewport: { width: 1200, height: 800 }, bypassCSP: true };
async function typeName(p, v, key = 'Enter') { await p.click('#nameIn'); await p.keyboard.press('Control+a'); await p.keyboard.type(v); await p.keyboard.press(key); await sleep(300); }
async function openPanel(p) {
  if (!(await p.evaluate(() => document.getElementById('whoPanel').hidden))) return;
  for (const c of await p.$$('.panel:not([hidden]):not(#whoPanel) [data-close]')) await c.click();
  await p.click('#who'); await sleep(300);
}
const b = await firefox.launch();
try {
  const ctx = await b.newContext(opts);
  const p = await ctx.newPage(); watch(p, 'A');
  await p.goto(BASE + '/');
  await p.waitForFunction(() => document.body.classList.contains('local'), null, { timeout: 10000 }).catch(() => {});
  ok('page boots in Firefox', await p.evaluate(() => document.body.classList.contains('local')));
  await openPanel(p);
  ok('panel opens', await p.evaluate(() => !document.getElementById('whoPanel').hidden && document.getElementById('who').getAttribute('aria-expanded') === 'true'));
  const n0 = await p.inputValue('#nameIn');
  ok('name field shows a name', n0.trim().length > 0, JSON.stringify(n0));
  await typeName(p, 'Ada Lovelace');
  ok('typed name is kept (wb.name)', (await ls(p, 'wb.name')) === 'Ada Lovelace', JSON.stringify(await ls(p, 'wb.name')));
  ok('typed name shows in the people button', /Ada Lovelace/.test(await p.textContent('#whoText')), JSON.stringify(await p.textContent('#whoText')));
  await typeName(p, '   ', 'Tab');
  const nBlank = await ls(p, 'wb.name');
  ok('a blank name rolls a new one', !!nBlank && nBlank.trim().length > 0 && nBlank !== 'Ada Lovelace', JSON.stringify(nBlank));
  const h0 = Number(await p.inputValue('#hueIn'));
  await p.focus('#hueIn'); for (let i = 0; i < 5; i++) await p.keyboard.press('ArrowRight');
  await p.dispatchEvent('#hueIn', 'change'); await sleep(300);
  const h1 = await ls(p, 'wb.hue');
  ok('colour change is kept (wb.hue)', h1 !== null && Number(h1) === Number(await p.inputValue('#hueIn')) && Number(h1) !== h0, `${h0}->${h1}`);
  const before = await ls(p, 'wb.name');
  await p.click('#reroll'); await sleep(300);
  const nr = await ls(p, 'wb.name');
  ok('New name picks another name and keeps it', nr && nr !== before && (await p.inputValue('#nameIn')) === nr, JSON.stringify(nr));
  await typeName(p, 'Ada Lovelace');
  await p.reload(); await sleep(1500);
  ok('name survives reload (button)', /Ada Lovelace/.test(await p.textContent('#whoText')), JSON.stringify(await p.textContent('#whoText')));
  await openPanel(p);
  ok('name survives reload (panel field)', (await p.inputValue('#nameIn')) === 'Ada Lovelace', JSON.stringify(await p.inputValue('#nameIn')));
  ok('colour survives reload (panel slider)', (await p.inputValue('#hueIn')) === h1, `${await p.inputValue('#hueIn')} vs ${h1}`);
  console.log('BOX', JSON.stringify(await p.evaluate(() => { const r = e => { const b = document.getElementById(e).getBoundingClientRect(); return [Math.round(b.x), Math.round(b.y), Math.round(b.width), Math.round(b.height)]; }; return { who: r('who'), panel: r('whoPanel'), list: r('whoList'), items: [...document.querySelectorAll('#whoList li')].map(l => l.className + ':' + l.textContent.slice(0, 30)) }; })));
  await p.click('#whoPanel [data-close]'); await sleep(200);
  ok('Close button shuts the panel', await p.evaluate(() => document.getElementById('whoPanel').hidden && document.getElementById('who').getAttribute('aria-expanded') === 'false'));
  await p.click('#shareBtn'); await sleep(300); await p.click('#shareNow');
  await p.waitForURL(/\/b\//, { timeout: 10000 }).catch(() => {});
  await p.waitForFunction(() => document.getElementById('status').classList.contains('ok'), null, { timeout: 10000 }).catch(() => {});
  const url = p.url();
  ok('share connects', /\/b\//.test(url) && await p.evaluate(() => document.getElementById('status').classList.contains('ok')));
  const c2 = await b.newContext(opts); const q = await c2.newPage(); watch(q, 'B');
  await q.goto(url);
  await q.waitForFunction(() => document.getElementById('status').classList.contains('ok'), null, { timeout: 10000 }).catch(() => {});
  await sleep(1500);
  await openPanel(q);
  ok('second person sees the typed name', /Ada Lovelace/.test(await q.textContent('#whoList')), JSON.stringify((await q.textContent('#whoList')).slice(0, 120)));
  await typeName(q, 'Grace Hopper');
  await sleep(1500); await openPanel(p);
  ok('a rename reaches the other person', /Grace Hopper/.test(await p.textContent('#whoList')), JSON.stringify((await p.textContent('#whoList')).slice(0, 120)));
  await typeName(q, 'Bob‮txt.exe', 'Tab');
  await sleep(1500);
  const wl = await p.textContent('#whoList');
  ok('a bidi override typed into a name is taken out for others (F27g)', /Bobtxt\.exe/.test(wl) && !/‮/.test(wl), JSON.stringify(wl.slice(0, 120)));
  await c2.close(); await ctx.close();
} catch (e) { errs.push('harness: ' + e.stack); } finally { await b.close(); }
console.log(res.join('\n'));
const real = errs.filter(e => !/favicon/.test(e));
console.log(`console/page errors: ${real.length}`);
for (const e of real.slice(0, 20)) console.log('  ' + e.slice(0, 600));
process.exit(res.some(r => r.startsWith('FAIL')) || real.length ? 1 : 0);
