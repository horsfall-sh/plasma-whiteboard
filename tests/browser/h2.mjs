// a stale wb.sharing key for a board the relay does not have must not trap "/".
import { createRequire } from 'module';
const { firefox } = createRequire(import.meta.url)((process.env.NODE_PATH ? process.env.NODE_PATH + '/' : '') + 'playwright-core');
const BASE = process.argv[2] || 'http://127.0.0.1:8799';
const res = [], errs = [];
const ok = (n, c, x = '') => res.push(`${c ? 'PASS' : 'FAIL'} ${n}${x ? ' ' + x : ''}`);
const sleep = ms => new Promise(r => setTimeout(r, ms));
const b = await firefox.launch();
try {
  const ctx = await b.newContext({ viewport: { width: 1200, height: 800 }, bypassCSP: true });
  const p = await ctx.newPage();
  p.on('pageerror', e => errs.push('pageerror: ' + e.message));
  await p.goto(BASE + '/'); await sleep(1500);
  await p.mouse.move(300, 300); await p.mouse.down();
  for (let i = 1; i <= 12; i++) await p.mouse.move(300 + 25 * i, 300 + 10 * i);
  await p.mouse.up(); await sleep(2000);
  const saved = await p.evaluate(() => localStorage.getItem('wb.local'));
  ok('a private stroke is saved', /"a":\[\{/.test(saved || ''));
  // a share cut short to a board that has since expired: a well-formed key the relay does not know
  await p.evaluate(() => localStorage.setItem('wb.sharing', 'AAAAAAAAAAAAAAAAAAAAAA'));
  await p.goto(BASE + '/'); await sleep(4000);
  const st = await p.evaluate(() => ({ path: location.pathname, sharing: localStorage.getItem('wb.sharing'), local: document.body.classList.contains('local'), msg: document.getElementById('msg').hidden ? '' : document.getElementById('msg').textContent, kept: /"a":\[\{/.test(localStorage.getItem('wb.local') || '') }));
  ok('lands on the private board at /', st.path === '/' && st.local && !st.msg, JSON.stringify(st));
  ok('the stale sharing key is gone', st.sharing === null);
  ok('the private stroke is still there', st.kept);
  await p.goto(BASE + '/'); await sleep(1500);
  ok('a second visit to / stays private (no loop)', await p.evaluate(() => location.pathname === '/' && document.body.classList.contains('local')));
} catch (e) { errs.push('harness: ' + e.stack); } finally { await b.close(); }
console.log(res.join('\n')); console.log('errors: ' + errs.length); for (const e of errs) console.log('  ' + e.slice(0, 400));
process.exit(res.some(r => r.startsWith('FAIL')) || errs.length ? 1 : 0);
