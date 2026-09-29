import { spawn } from 'node:child_process';
import { createHash } from 'node:crypto';
import { createReadStream, existsSync } from 'node:fs';
import { appendFile, mkdir, readFile, rm, stat, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { PROFILES, startFixtureWorker } from './fixture-server.mjs';

const MIB = 1024 * 1024;
const ALL_CLIENTS = ['fetchpath-http', 'fetchpath-cli', 'curl', 'aria2', 'aria2-x16', 'wget2'];
const exe = name => (process.platform === 'win32' ? `${name}.exe` : name);

function parseSize(text) {
  const match = /^(\d+)(b|k|kib|m|mib|g|gib)?$/i.exec(text ?? '');
  if (!match) throw new Error(`bad size: ${text}`);
  const unit = (match[2] ?? 'b').toLowerCase();
  return Number(match[1]) * ({ b: 1, k: 1024, kib: 1024, m: MIB, mib: MIB, g: 1024 * MIB, gib: 1024 * MIB }[unit]);
}

function options(argv) {
  const values = {
    repetitions: 5, size: 8 * MIB, seed: 15015, outputDir: null, corpus: 'fixture', profiles: ['unshaped', ...PROFILES], clients: ['fetchpath-http', 'curl'],
    httpBench: process.env.FETCHPATH_HTTP_BENCH ?? null, cli: process.env.FETCHPATH_CLI ?? null, aria2c: process.env.ARIA2C ?? null, wget2: process.env.WGET2 ?? null,
    keepFiles: false, timeoutS: 300, corpusFile: null,
  };
  for (let i = 0; i < argv.length; i += 1) {
    const key = argv[i];
    if (key === '--keep-files') { values.keepFiles = true; continue; }
    const value = argv[i + 1]; i += 1;
    if (value === undefined) throw new Error(`${key} needs a value`);
    if (key === '--repetitions') values.repetitions = Number(value);
    else if (key === '--size') values.size = parseSize(value);
    else if (key === '--seed') values.seed = Number(value);
    else if (key === '--output-dir') values.outputDir = value;
    else if (key === '--profile') values.profiles = value.split(',');
    else if (key === '--clients') values.clients = value.split(',');
    else if (key === '--corpus') values.corpus = value;
    else if (key === '--corpus-file') values.corpusFile = value;
    else if (key === '--http-bench') values.httpBench = value;
    else if (key === '--cli') values.cli = value;
    else if (key === '--aria2c') values.aria2c = value;
    else if (key === '--wget2') values.wget2 = value;
    else if (key === '--timeout-s') values.timeoutS = Number(value);
    else throw new Error(`Unknown argument: ${key}`);
  }
  if (!values.outputDir) throw new Error('--output-dir is required');
  if (!['fixture', 'internet'].includes(values.corpus)) throw new Error('--corpus must be fixture or internet');
  if (!Number.isSafeInteger(values.repetitions) || values.repetitions < 1 || values.repetitions > 50) throw new Error('--repetitions must be an integer from 1 to 50');
  if (!Number.isSafeInteger(values.size) || values.size < 1 || values.size > 1024 * MIB) throw new Error('--size must be an integer from 1 to 1073741824 (suffixes k, m, g accepted)');
  if (!Number.isSafeInteger(values.seed) || values.seed < 0 || values.seed > 0xffffffff) throw new Error('--seed must be an unsigned 32-bit integer');
  for (const client of values.clients) if (!ALL_CLIENTS.includes(client)) throw new Error(`Unknown client ${client}; expected ${ALL_CLIENTS.join(', ')}`);
  if (!(values.timeoutS > 0)) throw new Error('--timeout-s must be positive');
  if (values.corpus === 'fixture') {
    for (const spec of values.profiles) { const name = spec.split('?')[0]; if (name !== 'unshaped' && !PROFILES.includes(name)) throw new Error(`Unknown profile ${name}; expected unshaped, ${PROFILES.join(', ')}`); }
  }
  return values;
}

function run(command, args, extra = {}) {
  return new Promise((resolveRun, reject) => {
    const child = spawn(command, args, { windowsHide: true, ...extra }); let stdout = ''; let stderr = '';
    child.stdout.on('data', data => { stdout += data; }); child.stderr.on('data', data => { stderr += data; });
    child.once('error', reject); child.once('close', code => resolveRun({ code, stdout, stderr }));
  });
}

async function firstLine(command, args) {
  try { const r = await run(command, args); return r.code === 0 ? (r.stdout || r.stderr).split(/\r?\n/, 1)[0].trim() : null; } catch { return null; }
}

/** Deterministic PRNG so pair order and bootstrap intervals are reproducible from --seed. */
function mulberry32(seed) {
  let a = seed >>> 0;
  return () => { a = (a + 0x6d2b79f5) >>> 0; let t = a; t = Math.imul(t ^ (t >>> 15), t | 1); t ^= t + Math.imul(t ^ (t >>> 7), t | 61); return ((t ^ (t >>> 14)) >>> 0) / 4294967296; };
}
function shuffled(items, random) {
  const copy = [...items];
  for (let i = copy.length - 1; i > 0; i -= 1) { const j = Math.floor(random() * (i + 1)); [copy[i], copy[j]] = [copy[j], copy[i]]; }
  return copy;
}

const quantile = (sorted, q) => { if (!sorted.length) return null; const pos = (sorted.length - 1) * q; const lo = Math.floor(pos); const hi = Math.ceil(pos); return sorted[lo] + (sorted[hi] - sorted[lo]) * (pos - lo); };
const median = values => quantile([...values].sort((a, b) => a - b), 0.5);
const round = (value, digits = 3) => (value === null || value === undefined ? value : Number(value.toFixed(digits)));

/** Median, min, max and a seeded percentile-bootstrap 95% interval of the median. */
function summarize(values, random) {
  const numbers = values.filter(v => typeof v === 'number' && Number.isFinite(v));
  if (!numbers.length) return null;
  const sorted = [...numbers].sort((a, b) => a - b);
  let ci = null;
  if (numbers.length >= 3) {
    const medians = [];
    for (let b = 0; b < 2000; b += 1) medians.push(median(numbers.map(() => numbers[Math.floor(random() * numbers.length)])));
    medians.sort((a, b) => a - b); ci = [round(quantile(medians, 0.025)), round(quantile(medians, 0.975))];
  }
  return { n: numbers.length, median: round(median(numbers)), min: round(sorted[0]), max: round(sorted[sorted.length - 1]), ci95: ci };
}

// Windows exposes child CPU time and peak working set only through a handle opened before the child exits,
// so one long-lived PowerShell process polls each child by PID and reports the last readings.
const SIDECAR = `
while (($line = [Console]::In.ReadLine()) -ne $null) {
  $id = [int]$line; $cpu = $null; $peak = $null
  try { $p = [System.Diagnostics.Process]::GetProcessById($id); $null = $p.Handle } catch { [Console]::Out.WriteLine('{"error":"process gone before it could be observed"}'); continue }
  while (-not $p.HasExited) { try { $p.Refresh(); $peak = $p.PeakWorkingSet64; $cpu = $p.TotalProcessorTime.TotalSeconds } catch {}; Start-Sleep -Milliseconds 20 }
  try { $cpu = $p.TotalProcessorTime.TotalSeconds } catch {}
  [Console]::Out.WriteLine((@{cpuSeconds=$cpu; peakBytes=$peak} | ConvertTo-Json -Compress))
}
`;

async function startSidecar(dir) {
  if (process.platform !== 'win32') return { reason: 'child CPU and peak memory are only collected on Windows by this harness', watch: async () => null, close() {} };
  const script = join(dir, 'observe-process.ps1'); await writeFile(script, SIDECAR);
  for (const shell of ['pwsh', 'powershell']) {
    let child;
    try { child = spawn(shell, ['-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass', '-File', script], { windowsHide: true }); } catch { continue; }
    const failed = new Promise(res => child.once('error', () => res(true)));
    const lines = []; let waiter = null; let buffer = '';
    child.stdout.on('data', data => { buffer += data; let nl; while ((nl = buffer.indexOf('\n')) >= 0) { lines.push(buffer.slice(0, nl).trim()); buffer = buffer.slice(nl + 1); if (waiter) waiter(); } });
    child.stderr.on('data', () => {});
    if (await Promise.race([failed, new Promise(res => setTimeout(() => res(false), 300))])) continue;
    return {
      reason: null, shell,
      async watch(pid) {
        child.stdin.write(`${pid}\n`);
        return { async result() {
          const deadline = Date.now() + 10000;
          while (!lines.length && Date.now() < deadline) await new Promise(res => { waiter = res; setTimeout(res, 200); });
          const line = lines.shift(); if (!line) return { error: 'observer gave no reading' };
          try { return JSON.parse(line); } catch { return { error: `unreadable observer output: ${line}` }; }
        } };
      },
      close() { child.kill(); },
    };
  }
  return { reason: 'no PowerShell found to observe child CPU and memory', watch: async () => null, close() {} };
}

async function sha256File(file) {
  const hash = createHash('sha256');
  for await (const chunk of createReadStream(file)) hash.update(chunk);
  return hash.digest('hex');
}

/** Requests a tiny endpoint every 100 ms until stopped, keeping the response times. */
function startProbe(url) {
  const times = []; let stopped = false; let failures = 0;
  const loop = (async () => {
    while (!stopped) {
      const started = performance.now();
      try { const response = await fetch(url, { cache: 'no-store', signal: AbortSignal.timeout(5000) }); await response.arrayBuffer(); times.push(performance.now() - started); } catch { failures += 1; }
      const wait = 100 - (performance.now() - started); if (wait > 0) await new Promise(res => setTimeout(res, wait));
    }
  })();
  return { async stop() { stopped = true; await loop; const sorted = [...times].sort((a, b) => a - b); return { count: sorted.length, failures, p50Ms: round(quantile(sorted, 0.5)), p95Ms: round(quantile(sorted, 0.95)), maxMs: round(sorted[sorted.length - 1] ?? null) }; } };
}

async function measureProcess(command, args, { sidecar, timeoutMs, env }) {
  const started = process.hrtime.bigint();
  const child = spawn(command, args, { windowsHide: true, env: env ?? process.env });
  let stdout = ''; let stderr = ''; let timedOut = false; let spawnError = null;
  child.stdout.on('data', data => { if (stdout.length < 65536) stdout += data; }); child.stderr.on('data', data => { if (stderr.length < 65536) stderr += data; });
  const watcher = child.pid ? await sidecar.watch(child.pid) : null;
  const timer = setTimeout(() => { timedOut = true; child.kill(); }, timeoutMs);
  const code = await new Promise(res => { child.once('error', error => { spawnError = error.message; res(null); }); child.once('close', c => res(c)); });
  clearTimeout(timer);
  const wallMs = Number(process.hrtime.bigint() - started) / 1e6;
  const observed = watcher ? await watcher.result() : null;
  return { code, stdout, stderr, wallMs, timedOut, spawnError, observed };
}

async function resolveTools(config) {
  const tools = {}; const skipped = {};
  const curl = await (async () => { for (const c of process.platform === 'win32' ? ['curl.exe', 'curl'] : ['curl']) { const v = await firstLine(c, ['--version']); if (v) return { command: c, version: v }; } return null; })();
  const optional = async (name, command, args, notFound) => {
    const v = await firstLine(command, args);
    if (v) tools[name] = { command, version: v }; else skipped[name] = notFound;
  };
  for (const client of config.clients) {
    if (client === 'curl') { if (curl) tools.curl = curl; else skipped.curl = 'not installed (curl was not found on PATH)'; }
    else if (client === 'fetchpath-http') {
      const path = resolve(config.httpBench ?? join('target', 'release', exe('fetchpath-http-bench')));
      if (existsSync(path)) tools[client] = { command: path, version: `${path} (${(await stat(path)).size} bytes, modified ${(await stat(path)).mtime.toISOString()})` };
      else skipped[client] = `binary not found at ${path}; build with: cargo build --release -p fetchpath-http --bin fetchpath-http-bench, or pass --http-bench`;
    } else if (client === 'fetchpath-cli') {
      const path = resolve(config.cli ?? join('target', 'release', exe('fetchpath')));
      if (existsSync(path)) tools[client] = { command: path, version: (await firstLine(path, ['--version'])) ?? `${path} (${(await stat(path)).size} bytes, modified ${(await stat(path)).mtime.toISOString()})` };
      else skipped[client] = `binary not found at ${path}; build with: cargo build --release -p fetchpath-cli, or pass --cli`;
    } else if (client === 'aria2' || client === 'aria2-x16') {
      if (!tools.aria2Base && !skipped.aria2Base) { const command = config.aria2c ?? 'aria2c'; const v = await firstLine(command, ['--version']); if (v) tools.aria2Base = { command, version: v }; else skipped.aria2Base = 'not installed (aria2c not on PATH; set ARIA2C or --aria2c)'; }
      if (tools.aria2Base) tools[client] = tools.aria2Base; else skipped[client] = skipped.aria2Base;
    } else if (client === 'wget2') await optional('wget2', config.wget2 ?? 'wget2', ['--version'], 'not installed (wget2 not on PATH; set WGET2 or --wget2)');
  }
  delete tools.aria2Base; delete skipped.aria2Base;
  return { tools, skipped };
}

function clientCommand(client, tool, url, file, config) {
  const timeout = String(config.timeoutS);
  if (client === 'fetchpath-http') return { command: tool.command, args: [url, file] };
  if (client === 'fetchpath-cli') return { command: tool.command, args: ['download', url, file, '--json'], env: { ...process.env, FETCHPATH_APP_DATA_DIR: join(resolve(config.outputDir), 'cli-data') } };
  if (client === 'curl') return { command: tool.command, args: ['--disable', '--noproxy', '*', '--max-time', timeout, '--http1.1', '--location', '--silent', '--show-error', '--output', file, url] };
  const dirAndName = ['--dir', resolve(file, '..'), '--out', file.split(/[\\/]/).pop()];
  const aria2 = ['--no-conf=true', '--all-proxy=', '--console-log-level=error', '--summary-interval=0', '--download-result=hide', '--allow-overwrite=true', '--auto-file-renaming=false', `--timeout=${timeout}`, ...dirAndName];
  if (client === 'aria2') return { command: tool.command, args: [...aria2, url] };
  if (client === 'aria2-x16') return { command: tool.command, args: [...aria2, '-x16', '-s16', '-k1M', url] };
  return { command: tool.command, args: ['--no-config', '--no-proxy', '--quiet', '--output-document', file, url] };
}

function transferSummary(client, stdout) {
  try {
    const json = JSON.parse(stdout);
    if (client === 'fetchpath-http') return { usedRanges: json.usedRanges, adaptive: json.adaptive, peakConcurrency: json.peakConcurrency, negotiatedProtocol: json.negotiatedProtocol, peakBufferedBytes: json.budget?.peakBufferedBytes };
    return json;
  } catch { return undefined; }
}

const config = options(process.argv.slice(2));
const outputDir = resolve(config.outputDir);
await mkdir(outputDir, { recursive: true });
await rm(join(outputDir, 'runs.jsonl'), { force: true });
const { tools, skipped } = await resolveTools(config);
const activeClients = config.clients.filter(client => tools[client]);
if (!activeClients.length) throw new Error(`no requested client is available: ${JSON.stringify(skipped)}`);
const sidecar = await startSidecar(outputDir);
const random = mulberry32(config.seed);

let fixture = null; let targets;
if (config.corpus === 'fixture') {
  fixture = await startFixtureWorker({ size: config.size });
  targets = config.profiles.map(spec => {
    const [name, query] = spec.split('?');
    const path = name === 'unshaped' ? '/files/stable' : `/p/${name}`;
    return { name: spec, url: `${fixture.baseUrl}${path}${query ? `?${query}` : ''}`, expectedSha256: fixture.hashes.stable, selfRecorded: false, probeUrl: fixture.probeUrl };
  });
} else {
  const corpusFile = config.corpusFile ?? new URL('internet-corpus.json', import.meta.url);
  const corpus = JSON.parse(await readFile(corpusFile, 'utf8'));
  targets = corpus.entries.map(entry => ({ name: entry.name, url: entry.url, expectedSha256: entry.sha256 ?? null, selfRecorded: false, probeUrl: corpus.probeUrl }));
}

if (tools['fetchpath-cli']) await mkdir(join(outputDir, 'cli-data'), { recursive: true });
const runs = []; const failuresLog = [];
try {
  if (tools['fetchpath-cli'] && fixture) {
    // The first CLI call starts the engine process; keep that cold start out of the measured runs.
    const warm = join(outputDir, 'warmup.bin'); await rm(warm, { force: true });
    const spec = clientCommand('fetchpath-cli', tools['fetchpath-cli'], targets[0].url, warm, config);
    await measureProcess(spec.command, spec.args, { sidecar: { watch: async () => null }, timeoutMs: config.timeoutS * 1000, env: spec.env });
    await rm(warm, { force: true });
  }
  for (let rep = 0; rep < config.repetitions; rep += 1) {
    for (const target of targets) {
      const order = shuffled(activeClients, random);
      for (const client of order) {
        const file = join(outputDir, `${client}-${target.name.replace(/[^a-z0-9-]+/gi, '_')}-${rep}.bin`);
        await rm(file, { force: true });
        if (fixture) await fixture.reset();
        const probe = startProbe(target.probeUrl);
        const spec = clientCommand(client, tools[client], target.url, file, config);
        const result = await measureProcess(spec.command, spec.args, { sidecar, timeoutMs: config.timeoutS * 1000, env: spec.env });
        const probeResult = await probe.stop();
        const server = fixture ? await fixture.snapshot() : null;
        const verifyStarted = process.hrtime.bigint(); let outputBytes = null; let sha = null;
        try { outputBytes = (await stat(file)).size; sha = await sha256File(file); } catch { /* missing output counts as failed */ }
        const verifyMs = Number(process.hrtime.bigint() - verifyStarted) / 1e6;
        if (sha && !target.expectedSha256 && result.code === 0) { target.expectedSha256 = sha; target.selfRecorded = true; }
        const verified = sha !== null && sha === target.expectedSha256;
        const size = outputBytes ?? config.size;
        const observed = result.observed && !result.observed.error ? result.observed : null;
        const run = {
          rep, profile: target.name, client, order: order.indexOf(client), ok: result.code === 0 && verified && !result.timedOut, exitCode: result.code,
          timedOut: result.timedOut || undefined, error: result.spawnError ?? undefined, stderr: result.code === 0 ? undefined : result.stderr.slice(0, 300) || undefined,
          wallMs: round(result.wallMs, 1), verifyMs: round(verifyMs, 1), timeToVerifiedMs: round(result.wallMs + verifyMs, 1),
          goodputMiBps: round(size / MIB / (result.wallMs / 1000), 2), outputBytes, verified,
          cpuSeconds: observed ? round(observed.cpuSeconds, 3) : null, peakMemoryMiB: observed?.peakBytes ? round(observed.peakBytes / MIB, 1) : null,
          connections: server?.connectionsUsed ?? null, connectionsAccepted: server?.connectionsAccepted ?? null, requests: server?.requests ?? null, rangeRequests: server?.rangeRequests ?? null,
          redirects: server?.redirects ?? null, stalledRequests: server?.stalledRequests ?? null, probe: probeResult, transfer: transferSummary(client, result.stdout),
        };
        if (observed && outputBytes) run.cpuSecondsPerGiB = round(observed.cpuSeconds / (outputBytes / (1024 * MIB)), 2);
        runs.push(run); await appendFile(join(outputDir, 'runs.jsonl'), `${JSON.stringify(run)}
`);
        if (!config.keepFiles) await rm(file, { force: true });
        if (!run.ok) failuresLog.push(`${client} on ${target.name} rep ${rep}: exit ${result.code}${result.timedOut ? ' (timed out)' : ''}${verified ? '' : ', output not verified'} ${result.stderr.slice(0, 120).trim()}`);
      }
    }
  }
} finally { sidecar.close(); if (fixture) await fixture.close(); }

const cpuReason = sidecar.reason ?? (runs.some(r => r.cpuSeconds !== null) ? null : 'observer returned no readings');
const summary = [];
for (const target of targets) for (const client of activeClients) {
  const mine = runs.filter(r => r.profile === target.name && r.client === client); const good = mine.filter(r => r.ok);
  const pick = key => good.map(r => r[key]);
  summary.push({
    profile: target.name, client, runs: mine.length, ok: good.length,
    timeToVerifiedMs: summarize(pick('timeToVerifiedMs'), random), wallMs: summarize(pick('wallMs'), random), goodputMiBps: summarize(pick('goodputMiBps'), random),
    cpuSecondsPerGiB: summarize(pick('cpuSecondsPerGiB'), random), peakMemoryMiB: summarize(pick('peakMemoryMiB'), random),
    connections: summarize(pick('connections'), random), requests: summarize(pick('requests'), random), probeP95Ms: summarize(good.map(r => r.probe.p95Ms), random),
  });
}

// Engine overhead: how much slower the full engine (queue, checkpoints, publication) is than the transfer layer alone.
// Both use TransferLimits::default() (segments 1-8 MiB, up to 4 lanes, 8 active requests, 32 MiB buffered) unless a rule sets max_connections.
const engineOverhead = tools['fetchpath-http'] && tools['fetchpath-cli'] ? targets.map(target => {
  const of = client => summary.find(x => x.profile === target.name && x.client === client)?.timeToVerifiedMs?.median ?? null;
  const [http, cli] = [of('fetchpath-http'), of('fetchpath-cli')];
  return { profile: target.name, transferLayerMs: http, engineMs: cli, engineOverMs: http !== null && cli !== null ? round(cli - http, 1) : null, ratio: http && cli ? round(cli / http, 2) : null };
}) : undefined;
const gitRev = (await firstLine('git', ['rev-parse', '--short', 'HEAD'])) ?? null; const gitDirty = (await run('git', ['status', '--porcelain'])).stdout.trim().length > 0;
const artifact = {
  schemaVersion: 3,
  purpose: config.corpus === 'fixture'
    ? 'Paired loopback runs on application-shaped fixture profiles. Loopback timings support relative comparison of scheduling behavior only, not an Internet speed claim. Packet loss is not emulated.'
    : 'Recorded internet corpus from one home connection. Not a speed claim.',
  config: { ...config, outputDir: undefined, profiles: config.corpus === 'fixture' ? config.profiles : undefined },
  tools: Object.fromEntries(Object.entries(tools).map(([name, tool]) => [name, tool.version])), skipped,
  measurement: {
    order: 'per repetition and profile, clients shuffled with a seeded PRNG (--seed)',
    timeToVerifiedMs: 'wall time of the tool process plus a SHA-256 pass over its output file after it exits',
    cpuAndMemory: cpuReason ?? 'user+kernel CPU seconds and peak working set of the directly spawned process, polled every 20 ms from a PowerShell observer; child processes and a separate engine process (fetchpath-cli) are not included',
    probe: 'GET of a tiny endpoint every 100 ms while the tool runs; fixture probe is a separate listener in the fixture worker thread',
    interval: 'median with a seeded percentile bootstrap 95% interval (2000 resamples), reported from 3 runs up; with few runs it understates the real uncertainty',
  },
  host: { platform: process.platform, node: process.version, git: gitRev ? `${gitRev}${gitDirty ? '+dirty' : ''}` : null },
  fixture: fixture ? { size: fixture.size, stableSha256: fixture.hashes.stable } : undefined,
  corpusHashes: config.corpus === 'internet' ? targets.map(t => ({ name: t.name, sha256: t.expectedSha256, selfRecorded: t.selfRecorded })) : undefined,
  limits: 'fetchpath-http and fetchpath-cli both use TransferLimits::default() (segment 8 MiB max, 1 MiB min, 4 lanes max, 8 active requests, 32 MiB buffered); the engine applies max_connections only when a rule sets it',
  engineOverhead, failures: failuresLog, summary, runs,
};
const artifactPath = join(outputDir, 'fetchpath-benchmark-raw.json');
await writeFile(artifactPath, `${JSON.stringify(artifact)}\n`);

const cell = stat2 => (stat2 ? `${stat2.median}${stat2.ci95 ? ` [${stat2.ci95[0]}-${stat2.ci95[1]}]` : ''}` : 'n/a');
const lines = ['| profile | client | ok/runs | time to verified s (median [95% CI]) | goodput MiB/s | CPU s/GiB | peak MiB | conns | reqs | probe p95 ms |', '|---|---|---|---|---|---|---|---|---|---|'];
const toSeconds = stat2 => (stat2 ? { ...stat2, median: round(stat2.median / 1000, 2), ci95: stat2.ci95 && stat2.ci95.map(v => round(v / 1000, 2)) } : null);
for (const s of summary) lines.push(`| ${s.profile} | ${s.client} | ${s.ok}/${s.runs} | ${cell(toSeconds(s.timeToVerifiedMs))} | ${cell(s.goodputMiBps)} | ${s.cpuSecondsPerGiB?.median ?? 'n/a'} | ${s.peakMemoryMiB?.median ?? 'n/a'} | ${s.connections?.median ?? 'n/a'} | ${s.requests?.median ?? 'n/a'} | ${s.probeP95Ms?.median ?? 'n/a'} |`);
process.stdout.write(`${lines.join('\n')}\n`);
if (engineOverhead) process.stdout.write(`engine vs transfer layer (median time to verified, ms): ${engineOverhead.map(e => `${e.profile} ${e.transferLayerMs} -> ${e.engineMs} (x${e.ratio})`).join('; ')}
`);
for (const [name, reason] of Object.entries(skipped)) process.stdout.write(`skipped ${name}: ${reason}\n`);
if (cpuReason) process.stdout.write(`cpu/memory not recorded: ${cpuReason}\n`);
for (const failure of failuresLog) process.stdout.write(`failed: ${failure}\n`);
process.stdout.write(`${artifactPath}\n`);
