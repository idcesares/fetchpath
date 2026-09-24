// Drives the Fetchpath desktop window through WebView2's DevTools port:
// add, pause, resume, kill and restart, recovery, history search and filters.
// Usage: node drive.mjs <exe> <outDir> <downloadDir> <serverBase>
import { spawn, execSync } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import crypto from 'node:crypto';

const [exe, outDir, downloadDir, base] = process.argv.slice(2);
fs.mkdirSync(outDir, { recursive: true });
fs.mkdirSync(downloadDir, { recursive: true });
const dataDir = path.join(process.env.APPDATA, 'app.fetchpath.desktop');
const transcript = [];
const note = (step, detail) => { transcript.push({ step, ...detail }); console.log(step, JSON.stringify(detail)); };
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

let app, ws, nextId = 1;
const pending = new Map();

async function launch(label) {
  app = spawn(exe, [], { env: { ...process.env, WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: '--remote-debugging-port=9333' }, stdio: 'ignore' });
  let target;
  for (let i = 0; i < 150 && !target; i++) {
    await sleep(200);
    try {
      const list = await (await fetch('http://127.0.0.1:9333/json')).json();
      target = list.find((t) => t.type === 'page');
    } catch {}
  }
  if (!target) throw new Error('no DevTools target');
  ws = new WebSocket(target.webSocketDebuggerUrl);
  await new Promise((r, j) => { ws.onopen = r; ws.onerror = j; });
  ws.onmessage = (m) => { const msg = JSON.parse(m.data); if (msg.id && pending.has(msg.id)) { pending.get(msg.id)(msg); pending.delete(msg.id); } };
  await waitFor(() => evaluate(`!!document.getElementById('queue-title')`), 'window ready');
  await sleep(800);
  note('launched', { label });
}

function send(method, params = {}) {
  const id = nextId++;
  ws.send(JSON.stringify({ id, method, params }));
  return new Promise((r) => pending.set(id, r));
}
async function evaluate(expression) {
  const res = await send('Runtime.evaluate', { expression, awaitPromise: true, returnByValue: true });
  if (res.result?.exceptionDetails) throw new Error(JSON.stringify(res.result.exceptionDetails));
  return res.result?.result?.value;
}
async function waitFor(check, what, ms = 60000) {
  const until = Date.now() + ms;
  while (Date.now() < until) { try { const v = await check(); if (v) return v; } catch {} await sleep(250); }
  throw new Error(`timed out waiting for ${what}`);
}
async function shot(name) {
  const res = await send('Page.captureScreenshot', { format: 'png' });
  fs.writeFileSync(path.join(outDir, `${name}.png`), Buffer.from(res.result.data, 'base64'));
}
const rows = () => evaluate(`Array.from(document.querySelectorAll('#job-list article[data-job-id]')).map(a => ({
  id: a.dataset.jobId, state: a.dataset.state,
  source: a.querySelector('.job-source')?.textContent?.trim() ?? '',
  actions: Array.from(a.querySelectorAll('button[data-action]')).map(b => b.dataset.action).sort(),
}))`);
const rowFor = async (file) => (await rows()).find((r) => r.source.endsWith(file));
async function click(selector) { await evaluate(`document.querySelector(${JSON.stringify(selector)}).click()`); }
async function action(file, act) {
  const row = await waitFor(async () => { const r = await rowFor(file); return r?.actions.includes(act) && r; }, `${act} on ${file}`);
  await click(`button[data-action="${act}"][data-job-id="${row.id}"]`);
}
async function add(file) {
  await click('#add-open');
  await waitFor(() => evaluate(`document.getElementById('add-dialog').open`), 'add dialog');
  await evaluate(`(() => {
    const set = (id, v) => { const el = document.getElementById(id); el.value = v; el.dispatchEvent(new Event('input', { bubbles: true })); el.dispatchEvent(new Event('change', { bubbles: true })); };
    set('url', ${JSON.stringify(base + '/' + file)});
    set('destination', ${JSON.stringify(path.join(downloadDir, file))});
  })()`);
  await sleep(300);
  await click('#start-download');
  await waitFor(async () => !!(await rowFor(file)), `row for ${file}`);
  note('added', { file });
}
const state = async (file) => (await rowFor(file))?.state;
const waitState = (file, states, ms) => waitFor(async () => { const s = await state(file); return states.includes(s) && s; }, `${file} in ${states}`, ms);
function kill(label) { execSync(`taskkill /F /PID ${app.pid}`, { stdio: 'ignore' }); ws?.close(); note('killed', { label }); }
function persisted() {
  const file = path.join(dataDir, 'queue-v1.json');
  if (!fs.existsSync(file)) return null;
  const q = JSON.parse(fs.readFileSync(file, 'utf8'));
  return q.records.map((r) => ({ source: r.displayUrl.split('/').pop(), state: r.view.state, kind: r.view.kind, action: r.view.action ?? null, restart: !!r.restartUrl, complete: r.view.bytesReceived === r.view.totalBytes }))
    .sort((a, b) => a.source.localeCompare(b.source));
}
const hash = (file) => crypto.createHash('sha256').update(fs.readFileSync(path.join(downloadDir, file))).digest('hex');

// --- walkthrough ---------------------------------------------------------
await launch('first start, empty data folder');
if (await evaluate(`!document.getElementById('onboarding').hidden`)) { await click('#dismiss-onboarding'); note('onboarding dismissed', {}); }
note('empty queue', { rows: (await rows()).length, summary: await evaluate(`document.getElementById('queue-summary').textContent`) });
await shot('01-empty');

await add('small.bin');
note('small completed', { state: await waitState('small.bin', ['completed'], 30000) });

await add('big.bin');
await waitState('big.bin', ['running'], 20000);
await sleep(2500);
await action('big.bin', 'pause');
note('big paused', { state: await waitState('big.bin', ['paused'], 20000), actions: (await rowFor('big.bin')).actions });
await shot('02-paused');
await action('big.bin', 'resume');
note('big resumed', { state: await waitState('big.bin', ['running', 'queued'], 20000) });
await sleep(2000);
await action('big.bin', 'pause');
await waitState('big.bin', ['paused'], 20000);

await add('second.bin');
await waitState('second.bin', ['running'], 20000);
await sleep(2500);
note('before kill', { rows: (await rows()).map(({ source, state }) => ({ source: source.split('/').pop(), state })) });
await shot('03-before-kill');
kill('while second.bin runs and big.bin is paused');
note('persisted after kill', { records: persisted() });
await sleep(1500);

await launch('restart after kill');
note('after restart', { rows: (await rows()).map(({ source, state, actions }) => ({ source: source.split('/').pop(), state, actions })) });
await shot('04-after-restart');
note('second recovered', { state: await waitState('second.bin', ['completed'], 60000) });
await action('big.bin', 'resume');
note('big completed', { state: await waitState('big.bin', ['completed'], 60000) });

// History: search and filters.
await evaluate(`(() => { const s = document.getElementById('queue-search'); s.value = 'second'; s.dispatchEvent(new Event('input', { bubbles: true })); })()`);
await sleep(500);
note('search "second"', { visible: (await evaluate(`Array.from(document.querySelectorAll('#job-list article[data-job-id]')).filter(a => !a.hidden && a.offsetParent !== null).map(a => a.querySelector('.job-source').textContent.split('/').pop())`)) });
await evaluate(`(() => { const s = document.getElementById('queue-search'); s.value = ''; s.dispatchEvent(new Event('input', { bubbles: true })); })()`);
await click('button[data-filter="completed"]');
await sleep(500);
note('filter completed', { visible: (await evaluate(`Array.from(document.querySelectorAll('#job-list article[data-job-id]')).filter(a => !a.hidden && a.offsetParent !== null).map(a => a.querySelector('.job-source').textContent.split('/').pop()).sort()`)) });
await shot('05-history-completed');
await click('button[data-filter="all"]');

const hashes = JSON.parse(fs.readFileSync(process.env.WALK_LOG + '.hashes.json', 'utf8'));
note('files match the source', Object.fromEntries(['small.bin', 'big.bin', 'second.bin'].map((f) => [f, hash(f) === hashes['/' + f]])));

kill('end, to check what survives a second restart');
await sleep(1500);
await launch('second restart');
note('history after second restart', { rows: (await rows()).map(({ source, state }) => ({ source: source.split('/').pop(), state })).sort((a, b) => a.source.localeCompare(b.source)) });
note('persisted at end', { records: persisted() });
kill('done');
fs.writeFileSync(path.join(outDir, 'transcript.json'), JSON.stringify(transcript, null, 2));
process.exit(0);
