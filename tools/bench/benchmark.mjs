import { spawn } from 'node:child_process';
import { createHash } from 'node:crypto';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { createFixtureServer } from './fixture-server.mjs';

function options(argv) {
  const values = { repetitions: 5, size: 8 * 1024 * 1024, seed: 15015, outputDir: null };
  for (let i = 0; i < argv.length; i += 1) {
    const key = argv[i]; const value = argv[i + 1];
    if (key === '--repetitions') values.repetitions = Number(value);
    else if (key === '--size') values.size = Number(value);
    else if (key === '--seed') values.seed = Number(value);
    else if (key === '--output-dir') values.outputDir = value;
    else throw new Error(`Unknown argument: ${key}`);
    i += 1;
  }
  if (!values.outputDir) throw new Error('--output-dir is required');
  if (!Number.isSafeInteger(values.repetitions) || values.repetitions < 1 || values.repetitions > 20) throw new Error('--repetitions must be an integer from 1 to 20');
  if (!Number.isSafeInteger(values.size) || values.size < 1 || values.size > 16 * 1024 * 1024) throw new Error('--size must be an integer from 1 to 16777216');
  if (!Number.isSafeInteger(values.seed) || values.seed < 0 || values.seed > 0xffffffff) throw new Error('--seed must be an unsigned 32-bit integer');
  return values;
}

function run(command, args) {
  return new Promise((resolveRun, reject) => {
    const child = spawn(command, args, { windowsHide: true }); let stdout = ''; let stderr = '';
    child.stdout.on('data', data => { stdout += data; }); child.stderr.on('data', data => { stderr += data; });
    child.once('error', reject); child.once('close', code => resolveRun({ code, stdout, stderr }));
  });
}

async function runCurl(args) {
  for (const command of process.platform === 'win32' ? ['curl.exe', 'curl'] : ['curl']) {
    try { return { command, ...(await run(command, args)) }; } catch (error) { if (error.code !== 'ENOENT') throw error; }
  }
  throw new Error('curl was not found on PATH');
}

function nextRandom(state) {
  return (Math.imul(state, 1664525) + 1013904223) >>> 0;
}

async function measured(command, args) {
  const startedAt = new Date().toISOString(); const started = process.hrtime.bigint();
  const result = await run(command, args);
  return { ...result, startedAt, elapsedMs: Number(process.hrtime.bigint() - started) / 1e6 };
}

async function outputEvidence(file, expectedHash) {
  const bytes = await readFile(file); const sha256 = createHash('sha256').update(bytes).digest('hex');
  return { outputBytes: bytes.length, outputSha256: sha256, hashMatchesFixture: sha256 === expectedHash };
}

const config = options(process.argv.slice(2));
const outputDir = resolve(config.outputDir);
await mkdir(outputDir, { recursive: true });
const build = await measured('cargo', ['build', '--locked', '-p', 'fetchpath-http', '--bin', 'fetchpath-http-bench']);
if (build.code !== 0) throw new Error(`benchmark client build failed: ${build.stderr}`);
const client = resolve('target', 'debug', process.platform === 'win32' ? 'fetchpath-http-bench.exe' : 'fetchpath-http-bench');
const fixture = await createFixtureServer({ size: config.size });
try {
  const curlInfo = await runCurl(['--version']);
  if (curlInfo.code !== 0) throw new Error(`curl --version failed: ${curlInfo.stderr}`);
  const curlCommand = curlInfo.command;
  const versions = {
    curl: curlInfo.stdout.split(/\r?\n/, 1)[0],
    cargo: (await run('cargo', ['--version'])).stdout.trim(),
    rustc: (await run('rustc', ['--version'])).stdout.trim(),
  };
  const pairs = [];
  let randomState = config.seed;
  for (let index = 0; index < config.repetitions; index += 1) {
    randomState = nextRandom(randomState);
    const order = randomState & 1 ? ['fetchpath', 'curl'] : ['curl', 'fetchpath'];
    const pair = { index, order, runs: {} };
    for (const name of order) {
      const file = join(outputDir, `${name}-${index}.bin`);
      if (name === 'curl') {
        const result = await measured(curlCommand, ['--disable', '--noproxy', '*', '--max-time', '60', '--http1.1', '--silent', '--show-error', '--output', file, '--write-out', '{"http_version":"%{http_version}","http_code":%{http_code},"size_download":%{size_download},"time_total":%{time_total}}', `${fixture.baseUrl}/files/stable`]);
        pair.runs.curl = { startedAt: result.startedAt, elapsedMs: result.elapsedMs, exitCode: result.code, curlMetrics: JSON.parse(result.stdout), ...(await outputEvidence(file, fixture.hashes.stable)), stderr: result.stderr || undefined };
      } else {
        const result = await measured(client, [`${fixture.baseUrl}/files/stable`, file]);
        pair.runs.fetchpath = { startedAt: result.startedAt, elapsedMs: result.elapsedMs, exitCode: result.code, transfer: JSON.parse(result.stdout), ...(await outputEvidence(file, fixture.hashes.stable)), stderr: result.stderr || undefined };
      }
    }
    pairs.push(pair);
  }
  const artifact = {
    schemaVersion: 2,
    purpose: 'Paired localhost correctness and instrumentation evidence; loopback timings do not support an Internet speed claim.',
    fixture: { baseUrl: fixture.baseUrl, size: fixture.size, stableSha256: fixture.hashes.stable, stableEtag: fixture.etags.stable },
    config,
    versions,
    build: { elapsedMs: build.elapsedMs, exitCode: build.code },
    pairs,
  };
  const artifactPath = join(outputDir, 'fetchpath-benchmark-raw.json');
  await writeFile(artifactPath, `${JSON.stringify(artifact, null, 2)}\n`);
  process.stdout.write(`${artifactPath}\n`);
} finally { await fixture.close(); }
