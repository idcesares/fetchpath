import http from 'node:http';
import { spawn } from 'node:child_process';
import { mkdir, writeFile } from 'node:fs/promises';
import { resolve, join } from 'node:path';
import { createFixtureServer } from '../../bench/fixture-server.mjs';

const repositoryRoot = resolve(import.meta.dirname, '..', '..', '..');
const executable = process.env.FETCHPATH_HTTP_SPIKE_EXE
  ? resolve(process.env.FETCHPATH_HTTP_SPIKE_EXE)
  : join(repositoryRoot, 'work', 'http-backend-target', 'debug', 'fetchpath-http-spike.exe');
const output = join(repositoryRoot, 'work', 'http-backend-evidence');

function run(args) {
  return new Promise((resolveRun, reject) => {
    const child = spawn(executable, args, { windowsHide: true });
    let stdout = '';
    let stderr = '';
    child.stdout.on('data', value => { stdout += value; });
    child.stderr.on('data', value => { stderr += value; });
    child.on('error', reject);
    child.on('close', code => resolveRun({ args, code, stdout: stdout.trim(), stderr: stderr.trim() }));
  });
}

function listen(server) {
  return new Promise((resolveListen, reject) => {
    server.once('error', reject);
    server.listen(0, '127.0.0.1', () => resolveListen(server.address().port));
  });
}

function close(server) {
  return new Promise((resolveClose, reject) => {
    server.close(error => error ? reject(error) : resolveClose());
  });
}

await mkdir(output, { recursive: true });
const fixture = await createFixtureServer({ size: 1024 * 1024 });
let proxyRequest = null;
const proxy = http.createServer((req, res) => {
  proxyRequest = { method: req.method, url: req.url };
  res.writeHead(502, { 'Content-Length': '0' });
  res.end();
});
const slow = http.createServer((req, res) => {
  res.writeHead(200, { 'Content-Length': 512 * 1024 });
  let sent = 0;
  const timer = setInterval(() => {
    if (sent >= 512 * 1024) {
      clearInterval(timer);
      res.end();
      return;
    }
    res.write(Buffer.alloc(16 * 1024, 0x61));
    sent += 16 * 1024;
  }, 10);
  req.on('close', () => clearInterval(timer));
});

const proxyPort = await listen(proxy);
const slowPort = await listen(slow);
try {
  const capabilities = await run(['capabilities']);
  const local = await run(['get', `${fixture.baseUrl}/files/stable`]);
  const proxied = await run([
    'get',
    `${fixture.baseUrl}/files/stable`,
    '--proxy',
    `http://127.0.0.1:${proxyPort}`,
  ]);
  const cancelled = await run([
    'get',
    `http://127.0.0.1:${slowPort}/slow`,
    '--cancel-after',
    '65536',
  ]);
  const tls = await run(['get', 'https://example.com/', '--no-proxy']);
  const evidence = {
    schemaVersion: 1,
    scope: 'Disposable Windows Rust/libcurl packaging and capability spike; not comparative performance evidence.',
    executable,
    capabilities,
    local,
    proxy: { run: proxied, observedRequest: proxyRequest },
    cancellation: cancelled,
    tls,
    interpretation: {
      h1: 'local fixture result is the direct HTTP/1.1 check',
      h2: 'reported only from packaged libcurl feature query; no controlled H2 endpoint is included',
      h3: 'reported only from packaged libcurl feature query; no controlled H3 endpoint is included',
      proxy: 'a local explicit proxy receives the absolute-form request and returns 502',
      cancellation: 'a slow local response is aborted after a bounded received-byte threshold',
    },
  };
  const path = join(output, 'http-backend-spike.json');
  await writeFile(path, `${JSON.stringify(evidence, null, 2)}\n`);
  console.log(path);
} finally {
  await Promise.allSettled([fixture.close(), close(proxy), close(slow)]);
}
