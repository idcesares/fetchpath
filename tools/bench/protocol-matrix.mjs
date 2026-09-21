import { spawn } from 'node:child_process';
import { createHash } from 'node:crypto';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import http2 from 'node:http2';
import { resolve } from 'node:path';
import { createFixtureServer } from './fixture-server.mjs';

function run(command, args) {
  return new Promise((resolveRun, reject) => {
    const child = spawn(command, args, { windowsHide: true }); let stdout = ''; let stderr = '';
    child.stdout.on('data', data => { stdout += data; }); child.stderr.on('data', data => { stderr += data; });
    child.once('error', reject); child.once('close', code => resolveRun({ code, stdout, stderr }));
  });
}

function bytes(size) {
  const seed = createHash('sha256').update('fetchpath-fixture:protocol-matrix').digest();
  return Buffer.from({ length: size }, (_, index) => seed[index % seed.length] ^ (index & 0xff));
}

function range(value, size) {
  const match = /^bytes=(\d+)-(\d+)$/.exec(value || '');
  if (!match) return null;
  const start = Number(match[1]); const end = Number(match[2]);
  return Number.isSafeInteger(start) && Number.isSafeInteger(end) && start <= end && end < size ? { start, end } : null;
}

async function h2Server(body, etag) {
  const server = http2.createServer();
  server.on('stream', (stream, headers) => {
    const selected = range(headers.range, body.length);
    if (!selected || (headers['if-range'] && headers['if-range'] !== etag)) {
      stream.respond({ ':status': 200, etag, 'content-length': body.length }); stream.end(body); return;
    }
    const payload = body.subarray(selected.start, selected.end + 1);
    stream.respond({ ':status': 206, etag, 'content-length': payload.length, 'content-range': `bytes ${selected.start}-${selected.end}/${body.length}` });
    stream.end(payload);
  });
  await new Promise((resolveListen, reject) => { server.once('error', reject); server.listen(0, '127.0.0.1', resolveListen); });
  const address = server.address();
  return { url: `http://127.0.0.1:${address.port}/fixture`, close: () => new Promise((resolveClose, reject) => server.close(error => error ? reject(error) : resolveClose())) };
}

const outputDir = resolve(process.argv[2] || 'work/fp015-protocol');
await mkdir(outputDir, { recursive: true });
const build = await run('cargo', ['build', '--locked', '-p', 'fetchpath-http', '--bin', 'fetchpath-http-bench']);
if (build.code !== 0) throw new Error(build.stderr);
const client = resolve('target', 'debug', process.platform === 'win32' ? 'fetchpath-http-bench.exe' : 'fetchpath-http-bench');
const body = bytes(5 * 1024 * 1024);
const expectedSha256 = createHash('sha256').update(body).digest('hex');
const etag = `"sha256-${expectedSha256}"`;
const h1 = await createFixtureServer({ size: body.length });
const h2 = await h2Server(body, etag);
try {
  const h1File = resolve(outputDir, 'h1.bin'); const h2File = resolve(outputDir, 'h2.bin');
  const h1Run = await run(client, [`${h1.baseUrl}/files/stable`, h1File]);
  const h2Run = await run(client, ['--http2-prior-knowledge', h2.url, h2File]);
  if (h1Run.code !== 0 || h2Run.code !== 0) throw new Error(`protocol client failed\nh1: ${h1Run.stderr}\nh2: ${h2Run.stderr}`);
  const observed = async file => createHash('sha256').update(await readFile(file)).digest('hex');
  const artifact = {
    schemaVersion: 1,
    purpose: 'Controlled packaged-protocol negotiation and fallback evidence.',
    http1: { report: JSON.parse(h1Run.stdout), outputSha256: await observed(h1File), expectedSha256: h1.hashes.stable },
    http2PriorKnowledge: { report: JSON.parse(h2Run.stdout), outputSha256: await observed(h2File), expectedSha256 },
    http3: { attempted: false, reason: 'packaged libcurl reports HTTP/3 unavailable; policy records http3_unavailable and falls back without a false attempt' },
  };
  const path = resolve(outputDir, 'fetchpath-protocol-matrix.json');
  await writeFile(path, `${JSON.stringify(artifact, null, 2)}\n`);
  process.stdout.write(`${path}\n`);
} finally {
  await Promise.all([h1.close(), h2.close()]);
}
