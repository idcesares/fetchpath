import http from 'node:http';
import { createHash } from 'node:crypto';

function fixtureBytes(size, identity = 'stable') {
  const seed = createHash('sha256').update(`fetchpath-fixture:${identity}`).digest();
  const bytes = Buffer.allocUnsafe(size);
  for (let i = 0; i < size; i += 1) bytes[i] = seed[i % seed.length] ^ (i & 0xff);
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

function writeFile(res, bytes, etag, range, allowRange) {
  const headers = { ETag: etag, 'Accept-Ranges': allowRange ? 'bytes' : 'none', 'Content-Type': 'application/octet-stream' };
  if (range && allowRange) {
    const body = bytes.subarray(range.start, range.end + 1);
    res.writeHead(206, { ...headers, 'Content-Length': body.length, 'Content-Range': `bytes ${range.start}-${range.end}/${bytes.length}` });
    res.end(body);
    return;
  }
  res.writeHead(200, { ...headers, 'Content-Length': bytes.length });
  res.end(bytes);
}

/** Starts a deterministic localhost-only HTTP/1.1 fixture server. */
export async function createFixtureServer({ size = 1024 * 1024 } = {}) {
  if (!Number.isSafeInteger(size) || size < 1 || size > 16 * 1024 * 1024) throw new RangeError('size must be an integer from 1 to 16777216');
  const stable = fixtureBytes(size, 'stable');
  const changedA = fixtureBytes(size, 'changed-a');
  const changedB = fixtureBytes(size, 'changed-b');
  const hashes = Object.fromEntries(Object.entries({ stable, changedA, changedB }).map(([key, value]) => [key, createHash('sha256').update(value).digest('hex')]));
  const etags = { stable: `"sha256-${hashes.stable}"`, changedA: `"sha256-${hashes.changedA}"`, changedB: `"sha256-${hashes.changedB}"` };
  const server = http.createServer((req, res) => {
    const url = new URL(req.url, 'http://localhost');
    if (req.method !== 'GET') { res.writeHead(405, { Allow: 'GET' }); res.end(); return; }
    let bytes = stable;
    let etag = etags.stable;
    let allowRange = true;
    if (url.pathname === '/files/ignore-range') allowRange = false;
    else if (url.pathname === '/files/changed') {
      const variant = url.searchParams.get('variant') === 'b' ? 'changedB' : 'changedA';
      bytes = variant === 'changedB' ? changedB : changedA;
      etag = etags[variant];
    } else if (url.pathname === '/files/truncated') {
      res.writeHead(200, { ETag: etags.stable, 'Content-Type': 'application/octet-stream', 'Content-Length': stable.length });
      res.write(stable.subarray(0, Math.max(1, Math.floor(stable.length / 2))));
      setImmediate(() => res.destroy());
      return;
    } else if (url.pathname !== '/files/stable') { res.writeHead(404); res.end(); return; }
    const header = req.headers.range;
    // If-Range only permits a partial response for an exact strong validator.
    // Dates and weak ETags deliberately fall back to the complete representation.
    const ifRange = req.headers['if-range'];
    const conditionalRangeAllowed = !ifRange || (ifRange === etag && !ifRange.startsWith('W/'));
    const range = conditionalRangeAllowed ? parseRange(header, bytes.length) : null;
    if (header && !range && allowRange && conditionalRangeAllowed) {
      res.writeHead(416, { 'Content-Range': `bytes */${bytes.length}`, ETag: etag }); res.end(); return;
    }
    writeFile(res, bytes, etag, range, allowRange);
  });
  await new Promise((resolve, reject) => { server.once('error', reject); server.listen(0, '127.0.0.1', resolve); });
  const address = server.address();
  return { baseUrl: `http://127.0.0.1:${address.port}`, size, hashes, etags, close: () => new Promise((resolve, reject) => server.close(error => error ? reject(error) : resolve())) };
}
