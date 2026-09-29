import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import http from 'node:http';
import test from 'node:test';
import { createFixtureServer } from '../../tools/bench/fixture-server.mjs';

async function request(url, headers = {}) {
  const response = await fetch(url, { headers });
  return { response, bytes: Buffer.from(await response.arrayBuffer()) };
}
test('fixture size uses the same bounded input range as the runner', async () => {
  await assert.rejects(createFixtureServer({ size: 0 }), /1 to 1073741824/);
  await assert.rejects(createFixtureServer({ size: 1024 * 1024 * 1024 + 1 }), /1 to 1073741824/);
});
test('stable fixture serves valid byte ranges with a stable identity', async t => {
  const fixture = await createFixtureServer({ size: 1024 }); t.after(() => fixture.close());
  const { response, bytes } = await request(`${fixture.baseUrl}/files/stable`, { Range: 'bytes=10-19' });
  assert.equal(response.status, 206); assert.equal(response.headers.get('content-range'), 'bytes 10-19/1024');
  assert.equal(response.headers.get('etag'), fixture.etags.stable); assert.equal(bytes.length, 10);
  const full = await request(`${fixture.baseUrl}/files/stable`);
  assert.deepEqual(bytes, full.bytes.subarray(10, 20));
});
test('malformed and unsatisfiable ranges return 416', async t => {
  const fixture = await createFixtureServer({ size: 256 }); t.after(() => fixture.close());
  for (const range of ['bytes=abc-def', 'bytes=999-', 'bytes=1-2,3-4', 'items=0-1']) {
    const { response } = await request(`${fixture.baseUrl}/files/stable`, { Range: range });
    assert.equal(response.status, 416, range); assert.equal(response.headers.get('content-range'), 'bytes */256');
  }
});
test('ignore-range fixture deliberately returns the complete representation', async t => {
  const fixture = await createFixtureServer({ size: 100 }); t.after(() => fixture.close());
  const { response, bytes } = await request(`${fixture.baseUrl}/files/ignore-range`, { Range: 'bytes=5-9' });
  assert.equal(response.status, 200); assert.equal(bytes.length, 100); assert.equal(response.headers.get('accept-ranges'), 'none');
});
test('If-Range only honors the current strong ETag', async t => {
  const fixture = await createFixtureServer({ size: 1024 }); t.after(() => fixture.close());
  const current = await request(`${fixture.baseUrl}/files/changed?variant=b`);
  const stale = await request(`${fixture.baseUrl}/files/changed?variant=a`);
  const matched = await request(`${fixture.baseUrl}/files/changed?variant=b`, { Range: 'bytes=4-9', 'If-Range': current.response.headers.get('etag') });
  assert.equal(matched.response.status, 206); assert.equal(matched.bytes.length, 6);
  for (const validator of [stale.response.headers.get('etag'), `W/${current.response.headers.get('etag')}`]) {
    const fallback = await request(`${fixture.baseUrl}/files/changed?variant=b`, { Range: 'bytes=4-9', 'If-Range': validator });
    assert.equal(fallback.response.status, 200); assert.equal(fallback.bytes.length, 1024);
    assert.equal(fallback.response.headers.get('etag'), current.response.headers.get('etag'));
  }
});
test('content hash detects changed identities and truncated response fails', async t => {
  const fixture = await createFixtureServer({ size: 4096 }); t.after(() => fixture.close());
  const a = await request(`${fixture.baseUrl}/files/changed?variant=a`);
  const b = await request(`${fixture.baseUrl}/files/changed?variant=b`);
  assert.notEqual(a.response.headers.get('etag'), b.response.headers.get('etag'));
  assert.equal(createHash('sha256').update(a.bytes).digest('hex'), fixture.hashes.changedA);
  assert.notEqual(createHash('sha256').update(b.bytes).digest('hex'), fixture.hashes.changedA);
  await assert.rejects(fetch(`${fixture.baseUrl}/files/truncated`).then(response => response.arrayBuffer()));
});

// Shaped profiles. Sizes and timings are small so the suite stays fast.
function get(url, headers = {}, { abortAfterMs } = {}) {
  return new Promise((resolve, reject) => {
    const started = performance.now();
    const req = http.get(url, { agent: false, headers }, res => {
      const headerMs = performance.now() - started; const chunks = [];
      res.on('data', chunk => chunks.push(chunk));
      res.on('end', () => resolve({ status: res.statusCode, headers: res.headers, headerMs, totalMs: performance.now() - started, bytes: Buffer.concat(chunks) }));
      res.on('aborted', () => reject(new Error('aborted')));
    });
    req.on('error', reject);
    if (abortAfterMs) setTimeout(() => { req.destroy(); resolve({ abandoned: true }); }, abortAfterMs);
  });
}
const expectedByte = (identity, i) => createHash('sha256').update(`fetchpath-fixture:${identity}`).digest()[i % 32] ^ (i & 0xff);

test('per-connection limit scales with connections, per-client limit does not', async t => {
  const fixture = await createFixtureServer({ size: 1024 * 1024 }); t.after(() => fixture.close());
  const range = { Range: 'bytes=0-99999' };
  const timed = async path => { const started = performance.now(); await Promise.all([get(`${fixture.baseUrl}${path}`, range), get(`${fixture.baseUrl}${path}`, range)]); return performance.now() - started; };
  const perConnection = await timed('/p/per-connection-limit?rate=400000');
  const perClient = await timed('/p/per-client-limit?rate=400000');
  assert.ok(perConnection < 400, `two connections at 400 kB/s each moved 100 kB apiece in ${perConnection} ms`);
  assert.ok(perClient > 420, `one shared 400 kB/s bucket moved 200 kB in ${perClient} ms`);
  assert.ok(perClient > perConnection * 1.4);
});
test('delay profile holds the response headers back for every request', async t => {
  const fixture = await createFixtureServer({ size: 4096 }); t.after(() => fixture.close());
  for (let i = 0; i < 2; i += 1) {
    const shaped = await get(`${fixture.baseUrl}/p/delay?ms=150&rate=0`, { Range: 'bytes=0-9' });
    assert.equal(shaped.status, 206); assert.ok(shaped.headerMs >= 140, `headers after ${shaped.headerMs} ms`);
  }
  assert.ok((await get(`${fixture.baseUrl}/files/stable`, { Range: 'bytes=0-9' })).headerMs < 100);
});
test('stall profile stalls every Nth large range and can be abandoned', async t => {
  const fixture = await createFixtureServer({ size: 64 * 1024 }); t.after(() => fixture.close());
  const url = `${fixture.baseUrl}/p/stall?every=2&ms=300&minBytes=1000`; const range = { Range: 'bytes=0-9999' };
  const small = await get(url, { Range: 'bytes=0-9' }); assert.ok(small.totalMs < 250, 'ranges below minBytes never count or stall');
  const first = await get(url, range); assert.ok(first.totalMs < 250 && first.bytes.length === 10000);
  const second = await get(url, range); assert.ok(second.totalMs >= 280, `stalled request took ${second.totalMs} ms`); assert.equal(second.bytes.length, 10000);
  assert.equal(fixture.snapshot().stalledRequests, 1);
  const forever = await get(`${fixture.baseUrl}/p/stall?every=1&ms=0&minBytes=1000`, range, { abortAfterMs: 300 });
  assert.equal(forever.abandoned, true);
  // A body larger than the socket buffer must resume after the pause instead of waiting for a drain that already happened.
  const big = await createFixtureServer({ size: 4 * 1024 * 1024 }); t.after(() => big.close());
  const bigStall = await get(`${big.baseUrl}/p/stall?every=1&ms=200`, { Range: 'bytes=0-2097151' });
  assert.equal(bigStall.bytes.length, 2097152); assert.ok(bigStall.totalMs >= 180);
});
test('redirect profile sends every request to a second origin and counts it', async t => {
  const fixture = await createFixtureServer({ size: 4096 }); t.after(() => fixture.close());
  const first = await get(`${fixture.baseUrl}/p/redirect`, { Range: 'bytes=0-9' });
  assert.equal(first.status, 302); assert.ok(first.headers.location.startsWith(fixture.redirectTargetOrigin));
  assert.notEqual(new URL(first.headers.location).port, new URL(fixture.baseUrl).port);
  const followed = await get(first.headers.location, { Range: 'bytes=0-9' });
  assert.equal(followed.status, 206); assert.equal(followed.headers.etag, fixture.etags.stable);
  await get(`${fixture.baseUrl}/p/redirect`);
  assert.equal(fixture.snapshot().redirects, 2);
});
test('weak-etag and no-etag profiles allow ranges without a strong validator', async t => {
  const fixture = await createFixtureServer({ size: 4096 }); t.after(() => fixture.close());
  const weak = await get(`${fixture.baseUrl}/p/weak-etag`, { Range: 'bytes=10-19' });
  assert.equal(weak.status, 206); assert.equal(weak.headers.etag, `W/${fixture.etags.stable}`); assert.equal(weak.bytes.length, 10);
  const none = await get(`${fixture.baseUrl}/p/no-etag`, { Range: 'bytes=10-19' });
  assert.equal(none.status, 206); assert.equal(none.headers.etag, undefined); assert.equal(none.headers['accept-ranges'], 'bytes');
  const ifRange = await get(`${fixture.baseUrl}/p/weak-etag`, { Range: 'bytes=10-19', 'If-Range': weak.headers.etag });
  assert.equal(ifRange.status, 200, 'a weak validator never satisfies If-Range');
});
test('ranges near the top of a 1 GiB object are correct and per-request records are kept', async t => {
  const size = 1024 * 1024 * 1024; const fixture = await createFixtureServer({ size }); t.after(() => fixture.close());
  const start = size - 300000; // crosses the 256 KiB slice size and the pattern period
  const { status, headers, bytes } = await get(`${fixture.baseUrl}/p/weak-etag`, { Range: `bytes=${start}-` });
  assert.equal(status, 206); assert.equal(headers['content-range'], `bytes ${start}-${size - 1}/${size}`); assert.equal(bytes.length, 300000);
  for (const i of [0, 1, 255, 256, 299999]) assert.equal(bytes[i], expectedByte('stable', start + i));
  const snapshot = fixture.snapshot();
  assert.equal(snapshot.requests, 1); assert.equal(snapshot.connectionsUsed, 1); assert.equal(snapshot.bytesServed, 300000);
  assert.deepEqual(snapshot.log[0].range, `bytes=${start}-`);
  fixture.reset(); assert.equal(fixture.snapshot().requests, 0);
});
