// Process interruption, not OS crash or power loss. Uses only a scratch engine.
import { spawn } from 'node:child_process';
import { createHash } from 'node:crypto';
import { mkdir, readFile, readdir, writeFile } from 'node:fs/promises';
import { resolve, join } from 'node:path';
import { createFixtureServer } from './fixture-server.mjs';

const root = resolve(process.argv[2] ?? 'work/http-interruption');
const binary = resolve(process.argv[3] ?? `target/release/fetchpath${process.platform === 'win32' ? '.exe' : ''}`);
const home = join(root, 'engine');
await mkdir(home, { recursive: true });
if ((await readdir(root)).some(name => name !== 'engine') || (await readdir(home)).length) {
  throw new Error('Use an empty scratch output directory.');
}
const env = { ...process.env, FETCHPATH_APP_DATA_DIR: home, FETCHPATH_DATA_DIR: join(root, 'cache') };
const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));
function start(args) {
  const child = spawn(binary, args, { env, windowsHide: true });
  let stdout = ''; let stderr = '';
  child.stdout.on('data', data => { stdout += data; });
  child.stderr.on('data', data => { stderr += data; });
  child.done = new Promise((resolve, reject) => {
    child.once('error', reject);
    child.once('close', code => resolve({ code, stdout, stderr }));
  });
  return child;
}
async function checkpoint() {
  let latest = null;
  for (const name of await readdir(root)) {
    if (!/\.checkpoint\.\d+$/.test(name)) continue;
    let text;
    try { text = await readFile(join(root, name), 'utf8'); }
    catch (error) { if (error.code === 'ENOENT') continue; throw error; }
    const marker = text.lastIndexOf('checksum=');
    if (marker < 0 || createHash('sha256').update(text.slice(0, marker)).digest('hex') !== text.slice(marker + 9).trim()) continue;
    const values = Object.fromEntries(text.split('\n').map(line => line.split('=')));
    if (!latest || Number(values.generation) > latest.generation) {
      latest = { generation: Number(values.generation), committed: Number(values.committed_len) };
    }
  }
  return latest;
}
function served(snapshot) {
  const intervals = snapshot.log.filter(r => r.bytes > 0).map(r => {
    const start = Number(/^bytes=(\d+)-/.exec(r.range ?? '')?.[1] ?? 0);
    return [start, start + r.bytes];
  }).sort((a, b) => a[0] - b[0]);
  let bytes = 0; let end = 0;
  for (const [start, stop] of intervals) { bytes += Math.max(0, stop - Math.max(start, end)); end = Math.max(end, stop); }
  return bytes;
}
async function ready(child) {
  const until = Date.now() + 10000;
  while (Date.now() < until) {
    if (child.exitCode !== null) throw new Error('Explicit scratch engine exited before readiness.');
    if ((await start(['engine', 'status', '--json']).done).code === 0) return;
    await sleep(50);
  }
  throw new Error('Scratch engine did not become ready.');
}
const fixture = await createFixtureServer({ size: 256 * 1024 * 1024 });
let engine; let client;
try {
  engine = start(['engine']);
  await ready(engine);
  const destination = join(root, 'output.bin');
  const began = Date.now();
  client = start(['download', `${fixture.baseUrl}/p/per-connection-limit`, destination, '--json']);
  let before;
  const samples = [];
  while (Date.now() - began < 15000) {
    before = await checkpoint();
    samples.push([Date.now(), served(fixture.snapshot())]);
    while (samples.length > 1 && Date.now() - samples[0][0] > 1200) samples.shift();
    if (before?.committed > 0 && Date.now() - began > 4400) break;
    await sleep(30);
  }
  if (!before?.committed) throw new Error('No durable checkpoint before kill.');
    client.kill('SIGKILL'); await client.done;
  engine.kill('SIGKILL'); await engine.done;
  const afterKill = await checkpoint();
  if (!afterKill) throw new Error('Durable checkpoint missing after kill.');
  const bytesServed = served(fixture.snapshot()); // upper bound includes socket buffers
  const recentGoodput = (bytesServed - samples[0][1]) / ((Date.now() - samples[0][0]) / 1000);
  const lossEnvelope = Math.max(65536, 4 * recentGoodput);
  const queue = JSON.parse(await readFile(join(home, 'queue-v1.json'), 'utf8'));
  const id = queue.records.find(record => record.destination === destination)?.id;
  if (!id) throw new Error('Interrupted job was not persisted.');
  const beforeResume = fixture.snapshot().log.length;
  engine = start(['engine']);
  await ready(engine);
  // A query-free interrupted job is queued automatically by engine recovery.
  const deadline = Date.now() + 45000;
  let digest = null;
  while (Date.now() < deadline) {
    try { digest = createHash('sha256').update(await readFile(destination)).digest('hex'); break; }
    catch (error) { if (error.code !== 'ENOENT') throw error; }
    await sleep(50);
  }
  const resumed = fixture.snapshot().log.slice(beforeResume);
  const firstRange = resumed.find(r => r.status === 206)?.range;
  const resumeOffset = Number(/^bytes=(\d+)-/.exec(firstRange ?? '')?.[1] ?? 0);
  const elapsedS = (Date.now() - began) / 1000;
  const lossUpperBound = Math.max(0, bytesServed - afterKill.committed);
  const result = {
    profile: '256 MiB, per-connection 8 MiB/s, process killed after a durable checkpoint',
    method: 'SIGKILL/TerminateProcess of explicitly spawned scratch engine; no graceful commit drain',
    bytesServedUpperBound: bytesServed, checkpointBeforeKill: before, checkpointAfterKill: afterKill,
    lossUpperBound, originRecentGoodput: recentGoodput, lossEnvelope, resumeOffset, verified: digest === fixture.hashes.stable,
    resumedFromCheckpoint: resumeOffset === afterKill.committed, elapsedS,
    limits: 'Served bytes conservatively include socket buffers and clipped bytes; rate is an origin-served useful-byte proxy, not receiver goodput. One controlled process-kill run; no OS-crash or power-loss claim.',
  };
  await writeFile(join(root, 'interruption.json'), JSON.stringify(result, null, 2) + '\n');
  console.log(JSON.stringify(result, null, 2));
  if (!result.verified || !result.resumedFromCheckpoint || lossUpperBound > lossEnvelope) {
    throw new Error('Interruption verification, checkpoint resume, or recent-goodput loss envelope failed.');
  }
} finally {
  if (client && client.exitCode === null) { client.kill('SIGKILL'); await client.done; }
  if (engine && engine.exitCode === null) { engine.kill('SIGKILL'); await engine.done; }
  await fixture.close();
}
