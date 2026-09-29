import http from 'node:http';
import { createHash } from 'node:crypto';
import { Worker, isMainThread, parentPort, workerData } from 'node:worker_threads';

export const MAX_FIXTURE_SIZE = 1024 * 1024 * 1024;
const MIB = 1024 * 1024;
const CHUNK = 256 * 1024;

/** Default parameters per shaped profile. Every one can be overridden in the query string. */
export const PROFILE_DEFAULTS = {
  'per-connection-limit': { rate: 8 * MIB },
  'per-client-limit': { rate: 16 * MIB },
  delay: { ms: 50, rate: 32 * MIB },
  stall: { every: 4, ms: 3000, after: 0.5, minBytes: 65536 },
  redirect: {},
  'weak-etag': {},
  'no-etag': {},
};
export const PROFILES = Object.keys(PROFILE_DEFAULTS);

// The fixture pattern has period 256 (32-byte seed, index & 0xff), so any range is a slice of one repeated block.
function patternFor(identity) {
  const seed = createHash('sha256').update(`fetchpath-fixture:${identity}`).digest();
  const block = Buffer.allocUnsafe(256);
  for (let i = 0; i < 256; i += 1) block[i] = seed[i % seed.length] ^ i;
  const big = Buffer.allocUnsafe(CHUNK + 256);
  for (let i = 0; i < big.length; i += 256) block.copy(big, i, 0, Math.min(256, big.length - i));
  return big;
}

/** Bytes [offset, offset + length) of the object, length <= CHUNK, without copying. */
function slice(pattern, offset, length) {
  const shift = offset & 255;
  return pattern.subarray(shift, shift + length);
}

function streamingSha256(pattern, size) {
  const hash = createHash('sha256');
  for (let offset = 0; offset < size; offset += CHUNK) hash.update(slice(pattern, offset, Math.min(CHUNK, size - offset)));
  return hash.digest('hex');
}

export function fixtureBytes(size, identity = 'stable') {
  const pattern = patternFor(identity); const bytes = Buffer.allocUnsafe(size);
  for (let offset = 0; offset < size; offset += CHUNK) slice(pattern, offset, Math.min(CHUNK, size - offset)).copy(bytes, offset);
  return bytes;
}

function parseRange(header, size) {
  if (!header || !header.startsWith('bytes=') || header.includes(',')) return null;
  const match = /^bytes=(\d*)-(\d*)$/.exec(header);
  if (!match) return null;
  const [, startText, endText] = match;
  if (!startText && !endText) return null;
  if (!startText) {
    const length = Number(endText);
    if (!Number.isSafeInteger(length) || length <= 0) return null;
    return { start: Math.max(0, size - length), end: size - 1 };
  }
  const start = Number(startText);
  const end = endText ? Number(endText) : size - 1;
  if (!Number.isSafeInteger(start) || !Number.isSafeInteger(end) || start > end || start >= size) return null;
  return { start, end: Math.min(end, size - 1) };
}

/** Time-based token bucket: take(n) returns how long the caller must wait before sending n more bytes. */
class Bucket {
  constructor(rate, burstMs = 20) { this.rate = rate; this.burstMs = burstMs; this.at = 0; }
  take(n) {
    const now = performance.now();
    if (this.at < now) this.at = now;
    this.at += (n / this.rate) * 1000;
    return Math.max(0, this.at - now - this.burstMs);
  }
}

const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));

function numberParam(url, profile, name) {
  const raw = url.searchParams.get(name);
  const value = raw === null ? PROFILE_DEFAULTS[profile]?.[name] : Number(raw);
  if (value !== undefined && !Number.isFinite(value)) throw new RangeError(`bad ${name}`);
  return value;
}

/**
 * Starts deterministic localhost-only HTTP/1.1 fixture listeners: the file server, a second origin that
 * `redirect` sends clients to, and an unshaped `/probe` endpoint on its own port for latency probes.
 * Shaped profiles live at /p/<profile>[?rate=&ms=&every=&after=&minBytes=]. Legacy /files/* paths are unshaped.
 */
export async function createFixtureServer({ size = 1024 * 1024 } = {}) {
  if (!Number.isSafeInteger(size) || size < 1 || size > MAX_FIXTURE_SIZE) throw new RangeError(`size must be an integer from 1 to ${MAX_FIXTURE_SIZE}`);
  const patterns = { stable: patternFor('stable'), changedA: patternFor('changed-a'), changedB: patternFor('changed-b') };
  const hashes = {}; const etags = {};
  for (const key of Object.keys(patterns)) {
    let cached = null;
    const value = () => (cached ??= streamingSha256(patterns[key], size));
    Object.defineProperty(hashes, key, { enumerable: true, get: value });
    Object.defineProperty(etags, key, { enumerable: true, get: () => `"sha256-${value()}"` });
  }
  void hashes.stable; // hash once up front so the first request never pays for it
  const stats = { connections: 0, requests: [], redirects: 0, rangeRequests: 0 };
  let nextConnection = 1; let originB = null; let stallCounter = 0;
  const buckets = new Map();
  const bucketFor = (key, rate) => { const full = `${key}@${rate}`; if (!buckets.has(full)) buckets.set(full, new Bucket(rate)); return buckets.get(full); };

  async function send(res, entry, pattern, start, end, { bucket, stall }) {
    // Streams bytes [start, end], paced by bucket, optionally stalling once part-way through.
    let offset = start; const stallAt = stall ? start + Math.max(1, Math.floor((end - start + 1) * stall.after)) : -1;
    let closed = false; const closedP = new Promise(resolve => res.once('close', () => { closed = true; resolve(); }));
    const sliceMax = bucket ? Math.max(1024, Math.min(CHUNK, Math.floor(bucket.rate / 100))) : CHUNK;
    let stalled = false;
    while (offset <= end && !closed) {
      let length = Math.min(sliceMax, end - offset + 1);
      if (stall && !stalled && offset < stallAt) length = Math.min(length, stallAt - offset);
      if (bucket) { const wait = bucket.take(length); if (wait > 1) await sleep(wait); }
      if (closed) break;
      res.write(slice(pattern, offset, length));
      offset += length; entry.bytes += length;
      if (stall && !stalled && offset >= stallAt && offset <= end) {
        stalled = true; entry.stalled = true;
        await Promise.race([closedP, stall.ms > 0 ? sleep(stall.ms) : closedP]);
        if (closed) break;
      }
      if (res.writableNeedDrain && !closed) await Promise.race([closedP, new Promise(resolve => res.once('drain', resolve))]);
    }
    if (!closed) res.end();
  }

  function handler(listener) {
    return async (req, res) => {
      const socket = req.socket; const url = new URL(req.url, 'http://localhost');
      const entry = { conn: socket.fixtureId, method: req.method, path: url.pathname, range: req.headers.range ?? null, status: 0, bytes: 0, redirect: false, stalled: false };
      const finish = status => { entry.status = status; stats.requests.push(entry); };
      try {
        if (req.method !== 'GET') { res.writeHead(405, { Allow: 'GET' }); res.end(); finish(405); return; }
        let identity = 'stable'; let allowRange = true; let profile = null; let etag = etags.stable; let pattern = patterns.stable;
        const match = /^\/p\/([a-z-]+)$/.exec(url.pathname);
        if (match) {
          profile = match[1];
          if (!(profile in PROFILE_DEFAULTS) && profile !== 'redirect-target') { res.writeHead(404); res.end(); finish(404); return; }
        } else if (url.pathname === '/files/ignore-range') allowRange = false;
        else if (url.pathname === '/files/changed') {
          identity = url.searchParams.get('variant') === 'b' ? 'changedB' : 'changedA'; pattern = patterns[identity]; etag = etags[identity];
        } else if (url.pathname === '/files/truncated') {
          res.writeHead(200, { ETag: etags.stable, 'Content-Type': 'application/octet-stream', 'Content-Length': size });
          res.write(slice(patterns.stable, 0, Math.min(CHUNK, Math.max(1, Math.floor(size / 2)))));
          finish(200); setImmediate(() => res.destroy()); return;
        } else if (url.pathname !== '/files/stable') { res.writeHead(404); res.end(); finish(404); return; }
        if (profile === 'redirect' && listener === 'a') {
          stats.redirects += 1; entry.redirect = true;
          res.writeHead(302, { Location: `${originB}/p/redirect-target`, 'Content-Length': 0 }); res.end(); finish(302); return;
        }
        const headers = { 'Content-Type': 'application/octet-stream' };
        if (profile === 'weak-etag') headers.ETag = `W/${etag}`; else if (profile !== 'no-etag') headers.ETag = etag;
        headers['Accept-Ranges'] = allowRange ? 'bytes' : 'none';
        const header = req.headers.range;
        const ifRange = req.headers['if-range'];
        // If-Range only permits a partial response for an exact strong validator.
        const conditionalRangeAllowed = !ifRange || (headers.ETag !== undefined && ifRange === headers.ETag && !ifRange.startsWith('W/'));
        const range = conditionalRangeAllowed && allowRange ? parseRange(header, size) : null;
        if (header && !range && allowRange && conditionalRangeAllowed) {
          res.writeHead(416, { 'Content-Range': `bytes */${size}`, ...(headers.ETag ? { ETag: headers.ETag } : {}) }); res.end(); finish(416); return;
        }
        const start = range ? range.start : 0; const end = range ? range.end : size - 1; const length = end - start + 1;
        // Shaping. Per-connection buckets live on the socket, per-client buckets are keyed by remote address.
        let bucket = null; let stall = null;
        if (profile === 'per-connection-limit') { const rate = numberParam(url, profile, 'rate'); socket.buckets ??= new Map(); if (!socket.buckets.has(rate)) socket.buckets.set(rate, new Bucket(rate)); bucket = socket.buckets.get(rate); }
        else if (profile === 'per-client-limit') bucket = bucketFor(`client:${socket.remoteAddress}`, numberParam(url, profile, 'rate'));
        else if (profile === 'delay') {
          const ms = numberParam(url, profile, 'ms'); if (ms > 0) await sleep(ms);
          const rate = numberParam(url, profile, 'rate'); if (rate > 0) { socket.buckets ??= new Map(); if (!socket.buckets.has(rate)) socket.buckets.set(rate, new Bucket(rate)); bucket = socket.buckets.get(rate); }
        } else if (profile === 'stall') {
          if (length >= numberParam(url, profile, 'minBytes')) {
            stallCounter += 1;
            if (stallCounter % numberParam(url, profile, 'every') === 0) stall = { ms: numberParam(url, profile, 'ms'), after: numberParam(url, profile, 'after') };
          }
        }
        if (range) { stats.rangeRequests += 1; res.writeHead(206, { ...headers, 'Content-Length': length, 'Content-Range': `bytes ${start}-${end}/${size}` }); finish(206); }
        else { res.writeHead(200, { ...headers, 'Content-Length': length }); finish(200); }
        await send(res, entry, pattern, start, end, { bucket, stall });
      } catch (error) {
        if (!res.headersSent) { res.writeHead(500); res.end(); finish(500); } else res.destroy();
      }
    };
  }

  const listen = server => new Promise((resolve, reject) => { server.once('error', reject); server.listen(0, '127.0.0.1', () => resolve(server.address().port)); });
  const track = server => server.on('connection', socket => { socket.fixtureId = nextConnection; nextConnection += 1; stats.connections += 1; });
  const serverA = http.createServer(handler('a')); const serverB = http.createServer(handler('b'));
  const probe = http.createServer((req, res) => { res.writeHead(200, { 'Content-Length': 2, 'Cache-Control': 'no-store' }); res.end('ok'); });
  track(serverA); track(serverB);
  const [portA, portB, portProbe] = await Promise.all([listen(serverA), listen(serverB), listen(probe)]);
  originB = `http://127.0.0.1:${portB}`;
  const close = server => new Promise(resolve => { server.close(() => resolve()); server.closeAllConnections?.(); });
  return {
    baseUrl: `http://127.0.0.1:${portA}`, redirectTargetOrigin: originB, probeUrl: `http://127.0.0.1:${portProbe}/probe`,
    size, hashes, etags,
    /** Counters since the last reset. A connection counts when it is accepted, whether or not it sent a request. */
    snapshot() {
      const used = new Set(stats.requests.map(r => r.conn));
      return {
        connectionsAccepted: stats.connections, connectionsUsed: used.size, requests: stats.requests.length, rangeRequests: stats.rangeRequests,
        redirects: stats.redirects, bytesServed: stats.requests.reduce((sum, r) => sum + r.bytes, 0), stalledRequests: stats.requests.filter(r => r.stalled).length,
        log: stats.requests.map(r => ({ ...r })),
      };
    },
    reset() { stats.connections = 0; stats.requests = []; stats.redirects = 0; stats.rangeRequests = 0; stallCounter = 0; buckets.clear(); },
    close: () => Promise.all([close(serverA), close(serverB), close(probe)]).then(() => undefined),
  };
}

/** Runs a fixture server in a worker thread so its event loop never shares time with the harness's probes. */
export async function startFixtureWorker({ size }) {
  const worker = new Worker(new URL(import.meta.url), { workerData: { fixtureWorker: true, size } });
  let counter = 0; const pending = new Map();
  worker.on('message', ({ id, result, error }) => { const p = pending.get(id); if (!p) return; pending.delete(id); error ? p.reject(new Error(error)) : p.resolve(result); });
  worker.on('error', error => { for (const p of pending.values()) p.reject(error); pending.clear(); });
  const call = (cmd) => new Promise((resolve, reject) => { counter += 1; pending.set(counter, { resolve, reject }); worker.postMessage({ id: counter, cmd }); });
  const info = await call('info');
  return {
    ...info,
    snapshot: () => call('snapshot'), reset: () => call('reset'),
    async close() { await call('close'); await worker.terminate(); },
  };
}

if (!isMainThread && workerData?.fixtureWorker) {
  const fixture = await createFixtureServer({ size: workerData.size });
  const info = { baseUrl: fixture.baseUrl, redirectTargetOrigin: fixture.redirectTargetOrigin, probeUrl: fixture.probeUrl, size: fixture.size, hashes: { ...fixture.hashes }, etags: { ...fixture.etags } };
  parentPort.on('message', async ({ id, cmd }) => {
    try {
      let result;
      if (cmd === 'info') result = info; else if (cmd === 'snapshot') { result = fixture.snapshot(); delete result.log; } else if (cmd === 'reset') fixture.reset(); else if (cmd === 'close') await fixture.close();
      parentPort.postMessage({ id, result });
    } catch (error) { parentPort.postMessage({ id, error: String(error) }); }
  });
}
