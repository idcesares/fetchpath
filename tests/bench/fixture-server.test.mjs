import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import test from 'node:test';
import { createFixtureServer } from '../../tools/bench/fixture-server.mjs';

async function request(url, headers = {}) {
  const response = await fetch(url, { headers });
  return { response, bytes: Buffer.from(await response.arrayBuffer()) };
}
test('fixture size uses the same bounded input range as the runner', async () => {
  await assert.rejects(createFixtureServer({ size: 0 }), /1 to 16777216/);
  await assert.rejects(createFixtureServer({ size: 16 * 1024 * 1024 + 1 }), /1 to 16777216/);
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
