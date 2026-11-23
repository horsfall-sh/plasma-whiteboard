// the share question: no request reaches the relay until it is answered Share (Chromium).
import { createRequire } from 'module';
const { chromium } = createRequire(import.meta.url)((process.env.NODE_PATH ? process.env.NODE_PATH + '/' : '') + 'playwright-core');
const BASE = process.argv[2] || 'http://127.0.0.1:8799';
const res = [], errs = [];
const ok = (n, c, x = '') => res.push(`${c ? 'PASS' : 'FAIL'} ${n}${x ? ' ' + x : ''}`);
const sleep = ms => new Promise(r => setTimeout(r, ms));
const b = await chromium.launch();
try {
  const p = await (await b.newContext({ viewport: { width: 1200, height: 800 } })).newPage();
  const relay = [];
  p.on('request', r => { if (/\/api\//.test(r.url())) relay.push(r.method() + ' ' + new URL(r.url()).pathname); });
  p.on('websocket', w => relay.push('ws ' + new URL(w.url()).pathname));
  await p.goto(BASE + '/'); await sleep(1500);
  await p.click('#shareBtn'); await sleep(300);
  await p.click('#shareNow'); await sleep(300);
  const open = () => p.evaluate(() => !document.getElementById('shareAsk').hidden);
  ok('Share this board asks first', await open());
  ok('Cancel has focus', await p.evaluate(() => document.activeElement.id === 'shareCancel'));
  await p.keyboard.press('Tab');
  ok('Tab moves to Share', await p.evaluate(() => document.activeElement.id === 'shareGo'));
  await p.keyboard.press('Tab');
  ok('focus stays in the dialog', await p.evaluate(() => document.activeElement.id === 'shareCancel'));
  await p.keyboard.press('Escape'); await sleep(300);
  ok('Esc cancels', !(await open()));
  await p.click('#shareNow'); await sleep(200);
  await p.click('#shareCancel'); await sleep(1000);
  ok('Cancel keeps the board local', !(await open()) && relay.length === 0 && !/\/b\//.test(p.url()), JSON.stringify(relay));
  await p.click('#shareNow'); await sleep(200);
  await p.click('#shareGo');
  await p.waitForURL(/\/b\//, { timeout: 8000 }).catch(() => {});
  ok('Share connects', relay.some(r => r === 'POST /api/boards') && /\/b\//.test(p.url()), JSON.stringify(relay));
} catch (e) { errs.push('harness: ' + e.stack); } finally { await b.close(); }
console.log(res.join('\n')); console.log('errors: ' + errs.length); for (const e of errs) console.log('  ' + e.slice(0, 400));
process.exit(res.some(r => r.startsWith('FAIL')) || errs.length ? 1 : 0);
