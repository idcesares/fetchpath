// Isolates parallel scheduling on an identical retained prefix; no engine data.
import { spawn } from 'node:child_process';
import { createHash } from 'node:crypto';
import { createReadStream } from 'node:fs';
import { mkdir, readdir, writeFile, rm } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { performance } from 'node:perf_hooks';
import { fixtureBytes, startFixtureWorker } from './fixture-server.mjs';

const output = resolve(process.argv[2] ?? 'work/bench-resume');
await mkdir(output, { recursive: true });
if ((await readdir(output)).length) throw new Error('Use an empty scratch output directory.');
const binary = resolve(`target/release/fetchpath-http-bench${process.platform === 'win32' ? '.exe' : ''}`);
const size = 64 * 1024 * 1024;
const offset = 8 * 1024 * 1024;
const prefix = join(output, 'prefix.bin');
await writeFile(prefix, fixtureBytes(offset));
const fixture = await startFixtureWorker({ size });
const runs = [];
try {
  for (let rep = 0; rep < 5; rep += 1) {
    for (const lanes of rep % 2 ? [4, 1] : [1, 4]) {
      await fixture.reset();
      const file = join(output, `${rep}-${lanes}.bin`);
      const began = performance.now();
      const child = spawn(binary, ['--resume', String(offset), fixture.etags.stable,
        String(size), prefix, `${fixture.baseUrl}/p/per-connection-limit`, file], {
        windowsHide: true, env: { ...process.env, FETCHPATH_BENCH_MAX_LANES: String(lanes), FETCHPATH_HTTP_SCHEDULER: 'stream-first' },
      });
      let stdout = ''; let stderr = '';
      child.stdout.on('data', data => { stdout += data; });
      child.stderr.on('data', data => { stderr += data; });
      const code = await new Promise((resolve, reject) => {
        child.once('error', reject); child.once('close', resolve);
      });
      if (code !== 0) throw new Error(`Benchmark failed: ${stderr}`);
      const report = JSON.parse(stdout);
      const digest = createHash('sha256');
      for await (const bytes of createReadStream(file)) digest.update(bytes);
      const verified = digest.digest('hex') === fixture.hashes.stable;
      const stats = await fixture.snapshot();
      runs.push({ rep, lanes, timeToVerifiedMs: performance.now() - began, verified,
        bytes: report.bytes, peakConcurrency: report.peakConcurrency,
        requests: stats.requests, connections: stats.connectionsUsed });
      if (!verified) throw new Error('Resumed file does not match fixture hash.');
      await rm(file);
    }
  }
} finally { await fixture.close(); }
const median = values => [...values].sort((a, b) => a - b)[Math.floor(values.length / 2)];
const one = median(runs.filter(r => r.lanes === 1).map(r => r.timeToVerifiedMs));
const four = median(runs.filter(r => r.lanes === 4).map(r => r.timeToVerifiedMs));
const result = { profile: '64 MiB, 8 MiB retained, per-connection 8 MiB/s',
  method: 'Five alternating pairs, same binary and retained bytes, one-lane baseline versus default four-lane ceiling',
  singleMedianMs: one, adaptiveMedianMs: four, improvementPercent: 100 * (one - four) / one,
  limits: 'Transfer layer only; includes identical prefix copy and final hash pass; process recovery verified separately; no Internet speed claim.', runs };
await writeFile(join(output, 'resume.json'), JSON.stringify(result, null, 2) + '\n');
console.log(JSON.stringify({ ...result, runs: undefined }, null, 2));
if (result.improvementPercent < 10 || runs.some(r => r.peakConcurrency > r.lanes)) {
  throw new Error('Resume gain or lane budget gate failed.');
}
