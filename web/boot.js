import init, { start } from './page.js';
init().then(start, () => { document.getElementById('needsWasm').hidden = false; });
