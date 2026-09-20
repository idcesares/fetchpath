import { spawn } from 'node:child_process';
import { createHash } from 'node:crypto';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { createFixtureServer } from './fixture-server.mjs';

function options(argv) {
  const values = { repetitions: 3, size: 1024 * 1024, outputDir: null };
  for (let i = 0; i < argv.length; i += 1) {
    const key = argv[i]; const value = argv[i + 1];
    if (key === '--repetitions') values.repetitions = Number(value);
    else if (key === '--size') values.size = Number(value);
    else if (key === '--output-dir') values.outputDir = value;
    else throw new Error(`Unknown argument: ${key}`);
    i += 1;
  }
  if (!values.outputDir) throw new Error('--output-dir is required');
  if (!Number.isSafeInteger(values.repetitions) || values.repetitions < 1 || values.repetitions > 20) throw new Error('--repetitions must be an integer from 1 to 20');
  if (!Number.isSafeInteger(values.size) || values.size < 1 || values.size > 16 * 1024 * 1024) throw new Error('--size must be an integer from 1 to 16777216');
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

const config = options(process.argv.slice(2));
const outputDir = resolve(config.outputDir);
await mkdir(outputDir, { recursive: true });
const fixture = await createFixtureServer({ size: config.size });
try {
  const curlInfo = await runCurl(['--version']);
  if (curlInfo.code !== 0) throw new Error(`curl --version failed: ${curlInfo.stderr}`);
  const curlVersion = curlInfo.stdout.split(/\r?\n/, 1)[0];
  const runs = [];
  for (let index = 0; index < config.repetitions; index += 1) {
    const file = join(outputDir, `fetchpath-stable-${index}.bin`);
    const startedAt = new Date().toISOString(); const started = process.hrtime.bigint();
    const result = await runCurl(['--disable', '--noproxy', '*', '--max-time', '10', '--http1.1', '--silent', '--show-error', '--output', file, '--write-out', '{"http_version":"%{http_version}","http_code":%{http_code},"size_download":%{size_download},"time_total":%{time_total}}', `${fixture.baseUrl}/files/stable`]);
    const elapsedMs = Number(process.hrtime.bigint() - started) / 1e6;
    const bytes = await readFile(file); const sha256 = createHash('sha256').update(bytes).digest('hex');
    runs.push({ index, startedAt, curl: result.command, exitCode: result.code, curlMetrics: JSON.parse(result.stdout), elapsedMs, outputBytes: bytes.length, outputSha256: sha256, hashMatchesFixture: sha256 === fixture.hashes.stable, stderr: result.stderr || undefined });
  }
  const artifact = { schemaVersion: 1, purpose: 'Local HTTP/1.1 fixture sanity only; this is not comparative performance evidence.', fixture: { baseUrl: fixture.baseUrl, size: fixture.size, stableSha256: fixture.hashes.stable, stableEtag: fixture.etags.stable }, config, curlVersion, runs };
  const artifactPath = join(outputDir, 'fetchpath-benchmark-raw.json');
  await writeFile(artifactPath, `${JSON.stringify(artifact, null, 2)}\n`);
  process.stdout.write(`${artifactPath}\n`);
} finally { await fixture.close(); }
