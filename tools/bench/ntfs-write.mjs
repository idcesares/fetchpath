// Standalone write-order measurement (not part of `node --test`).
// Writes one file of --size bytes in 1 MiB positional writes in different orders, with and without set_len first:
//   sequential        offsets ascending
//   striped-4         4 lanes, each ascending through its own quarter, written round-robin (like 4 concurrent ranges)
//   shuffled          seeded random order of 8 MiB segments, ascending inside each segment
// Without preallocation, a write past the current end of a file makes NTFS zero-fill the gap up to that write
// (valid data length), so out-of-order writes can cost extra disk writes. The `preallocated` rows call
// ftruncate to the full size before the first write.
//   node tools/bench/ntfs-write.mjs [--size 256m] [--dir work/ntfs] [--repetitions 3] [--seed 15015] [--out result.json]
import { closeSync, fsyncSync, ftruncateSync, mkdirSync, openSync, rmSync, statSync, writeSync } from 'node:fs';
import { writeFileSync } from 'node:fs';
import { join, resolve } from 'node:path';

const MIB = 1024 * 1024;
const args = process.argv.slice(2); const opt = { size: 256 * MIB, dir: 'work/ntfs', repetitions: 3, seed: 15015, out: null };
for (let i = 0; i < args.length; i += 2) {
  const [key, value] = [args[i], args[i + 1]];
  if (key === '--size') { const m = /^(\d+)(k|m|g)?$/i.exec(value); opt.size = Number(m[1]) * ({ k: 1024, m: MIB, g: 1024 * MIB }[(m[2] ?? '').toLowerCase()] ?? 1); }
  else if (key === '--dir') opt.dir = value; else if (key === '--repetitions') opt.repetitions = Number(value);
  else if (key === '--seed') opt.seed = Number(value); else if (key === '--out') opt.out = value; else throw new Error(`Unknown argument ${key}`);
}
const CHUNK = MIB; const SEGMENT = 8 * MIB;
const chunks = Math.ceil(opt.size / CHUNK); const perSegment = SEGMENT / CHUNK; const segments = Math.ceil(chunks / perSegment);
function prng(seed) { let a = seed >>> 0; return () => { a = (a + 0x6d2b79f5) >>> 0; let t = a; t = Math.imul(t ^ (t >>> 15), t | 1); t ^= t + Math.imul(t ^ (t >>> 7), t | 61); return ((t ^ (t >>> 14)) >>> 0) / 4294967296; }; }
function order(name) {
  const list = [];
  if (name === 'sequential') for (let c = 0; c < chunks; c += 1) list.push(c);
  else if (name === 'striped-4') {
    const quarter = Math.ceil(chunks / 4);
    for (let step = 0; step < quarter; step += 1) for (let lane = 0; lane < 4; lane += 1) { const c = lane * quarter + step; if (c < chunks) list.push(c); }
  } else {
    const random = prng(opt.seed); const ids = Array.from({ length: segments }, (_, i) => i);
    for (let i = ids.length - 1; i > 0; i -= 1) { const j = Math.floor(random() * (i + 1)); [ids[i], ids[j]] = [ids[j], ids[i]]; }
    for (const s of ids) for (let k = 0; k < perSegment; k += 1) { const c = s * perSegment + k; if (c < chunks) list.push(c); }
  }
  return list;
}
const buffer = Buffer.alloc(CHUNK, 0xa5);
const ms = start => Number(process.hrtime.bigint() - start) / 1e6;
function measure(name, preallocate, dir) {
  const path = join(dir, `${name}-${preallocate ? 'pre' : 'nopre'}.bin`); rmSync(path, { force: true });
  const fd = openSync(path, 'w+'); let written = 0;
  const list = order(name);
  const total = process.hrtime.bigint();
  let t = process.hrtime.bigint(); if (preallocate) ftruncateSync(fd, opt.size); const setLenMs = ms(t);
  t = process.hrtime.bigint();
  for (const c of list) { const offset = c * CHUNK; const length = Math.min(CHUNK, opt.size - offset); written += writeSync(fd, buffer, 0, length, offset); }
  const writeMs = ms(t); t = process.hrtime.bigint(); fsyncSync(fd); const syncMs = ms(t);
  const totalMs = ms(total); closeSync(fd);
  const finalSize = statSync(path).size; rmSync(path, { force: true });
  return { order: name, preallocated: preallocate, setLenMs: +setLenMs.toFixed(1), writeMs: +writeMs.toFixed(1), syncMs: +syncMs.toFixed(1), totalMs: +totalMs.toFixed(1), bytesWritten: written, finalSize };
}
const dir = resolve(opt.dir); mkdirSync(dir, { recursive: true });
const cases = [['sequential', false], ['striped-4', false], ['shuffled', false], ['striped-4', true], ['shuffled', true]];
const results = [];
for (let rep = 0; rep < opt.repetitions; rep += 1) for (const [name, pre] of cases) results.push({ rep, ...measure(name, pre, dir) });
const median = v => { const s = [...v].sort((a, b) => a - b); return s[Math.floor(s.length / 2)]; };
const summary = cases.map(([name, pre]) => { const mine = results.filter(r => r.order === name && r.preallocated === pre); return { order: name, preallocated: pre, medianTotalMs: median(mine.map(r => r.totalMs)), medianWriteMs: median(mine.map(r => r.writeMs)), medianSyncMs: median(mine.map(r => r.syncMs)), medianSetLenMs: median(mine.map(r => r.setLenMs)), bytesWritten: mine[0].bytesWritten } });
const artifact = { size: opt.size, chunkBytes: CHUNK, segmentBytes: SEGMENT, repetitions: opt.repetitions, seed: opt.seed, platform: process.platform, node: process.version, dir, note: 'Wall times from synchronous positional writes plus one fsync. Physical disk bytes (zero-fill) are not observable from Node; the cost shows as time. Page cache and antivirus can dominate small differences.', summary, results };
if (opt.out) writeFileSync(opt.out, `${JSON.stringify(artifact)}\n`);
console.log('| order | preallocated | set_len ms | write ms | fsync ms | total ms (median of ' + opt.repetitions + ') | bytes written |\n|---|---|---|---|---|---|---|');
for (const s of summary) console.log(`| ${s.order} | ${s.preallocated ? 'yes' : 'no'} | ${s.medianSetLenMs} | ${s.medianWriteMs} | ${s.medianSyncMs} | ${s.medianTotalMs} | ${s.bytesWritten} |`);
