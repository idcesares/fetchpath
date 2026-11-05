// The share page's decryptor against the engine's encrypted stream (FP-117).
// crates/fetchpath-session/tests/fixtures/share-vectors.json pins SHA-256 of
// what the Rust engine writes. Here the same stream is rebuilt from the spec
// with node:crypto and must hash identically, so the page script decrypting it
// is decrypting the engine's format.
import assert from 'node:assert/strict';
import { createCipheriv, createHash, hkdfSync } from 'node:crypto';
import { readFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import test from 'node:test';

const page = createRequire(import.meta.url)('./page.cjs');
const vectors = JSON.parse(readFileSync(
  new URL('../../../../crates/fetchpath-session/tests/fixtures/share-vectors.json', import.meta.url), 'utf8'));
const KEY = Buffer.from(vectors.keyHex, 'hex');
const SALT = Buffer.from(vectors.saltHex, 'hex');
const LOG2 = 16;
const CHUNK = 1 << LOG2;
const SEALED = CHUNK + 16;

function plainOf(length) {
  return Buffer.from(Array.from({ length }, (_, i) => (i * 7 + 3) % 256));
}

function headerOf(log2 = LOG2, salt = SALT) {
  return Buffer.concat([Buffer.from('FPS1'), Buffer.from([1, log2]), salt]);
}

function sealer(key, header) {
  const okm = Buffer.from(hkdfSync('sha256', key, header.subarray(6), 'fetchpath-share-v1', 39));
  return (index, last, plain) => {
    const iv = Buffer.alloc(12);
    okm.copy(iv, 0, 32, 39);
    iv.writeUInt32BE(index, 7);
    iv[11] = last ? 1 : 0;
    const cipher = createCipheriv('aes-256-gcm', okm.subarray(0, 32), iv);
    cipher.setAAD(header);
    return Buffer.concat([cipher.update(plain), cipher.final(), cipher.getAuthTag()]);
  };
}

function encrypt(plain, { key = KEY, header = headerOf(), lastFlag = (_, last) => last } = {}) {
  const seal = sealer(key, header);
  const chunks = Math.max(1, Math.ceil(plain.length / CHUNK));
  const out = [header];
  for (let i = 0; i < chunks; i += 1) {
    out.push(seal(i, lastFlag(i, i === chunks - 1), plain.subarray(i * CHUNK, (i + 1) * CHUNK)));
  }
  return Buffer.concat(out);
}

async function decrypt(cipher, step = cipher.length || 1) {
  const decryptor = await page.createDecryptor(KEY);
  const pieces = [];
  for (let at = 0; at < cipher.length; at += step) {
    pieces.push(...await decryptor.push(cipher.subarray(at, at + step)));
  }
  pieces.push(...await decryptor.finish());
  return Buffer.concat(pieces);
}

const damaged = (reason) => (error) => error instanceof page.ShareError && error.message === reason;

test('the engine format: same bytes, and the page decrypts them', async () => {
  for (const { plainLen, cipherSha256 } of vectors.cases) {
    const plain = plainOf(plainLen);
    const cipher = encrypt(plain);
    assert.equal(createHash('sha256').update(cipher).digest('hex'), cipherSha256, `engine bytes for ${plainLen}`);
    assert.equal(cipher.length, 22 + plainLen + 16 * Math.max(1, Math.ceil(plainLen / CHUNK)));
    for (const step of [cipher.length || 1, 4096, 7919]) {
      assert.ok((await decrypt(cipher, step)).equals(plain), `${plainLen} bytes in pieces of ${step}`);
    }
  }
  assert.ok((await decrypt(encrypt(plainOf(70000)), 1000)).equals(plainOf(70000)));
});

test('a damaged, cut or extended stream is refused whole', async () => {
  const two = encrypt(plainOf(2 * CHUNK)); // an exact multiple: the last chunk is full
  const three = encrypt(plainOf(3 * CHUNK + 5));
  const attempts = {
    'trailing byte': Buffer.concat([two, Buffer.from([0])]),
    'trailing chunk': Buffer.concat([two, two.subarray(22, 22 + SEALED)]),
    'cut at a chunk boundary': two.subarray(0, 22 + SEALED),
    'cut inside a chunk': three.subarray(0, three.length - 3),
    'header only': two.subarray(0, 22),
    'nothing': Buffer.alloc(0),
    'last chunk flagged as not last': encrypt(plainOf(10), { lastFlag: () => false }),
    'a flipped bit': Buffer.from(two.map((byte, i) => (i === 30 ? byte ^ 1 : byte))),
    'chunks swapped': Buffer.concat([two.subarray(0, 22), two.subarray(22 + SEALED), two.subarray(22, 22 + SEALED)]),
    'another key': encrypt(plainOf(10), { key: Buffer.alloc(32, 8) }),
    'another salt in the header': Buffer.concat([headerOf(LOG2, Buffer.alloc(16, 1)), two.subarray(22)]),
  };
  for (const [what, cipher] of Object.entries(attempts)) {
    await assert.rejects(decrypt(cipher, 5000), damaged('damaged'), what);
  }
});

test('a header outside what readers accept is refused before any key use', async () => {
  const bad = (header) => Buffer.concat([header, Buffer.alloc(40)]);
  for (const header of [
    headerOf(15), headerOf(23), headerOf(0), headerOf(255),
    Buffer.concat([Buffer.from('FPS2'), Buffer.from([1, LOG2]), SALT]),
    Buffer.concat([Buffer.from('FPS1'), Buffer.from([2, LOG2]), SALT]),
  ]) {
    await assert.rejects(decrypt(bad(header)), damaged('format'));
  }
});

test('resume continues from the first chunk not opened yet', async () => {
  const plain = plainOf(5 * CHUNK + 9);
  const cipher = encrypt(plain);
  const decryptor = await page.createDecryptor(KEY);
  const got = [];
  // Two chunks and a half arrive, then the connection drops.
  got.push(...await decryptor.push(cipher.subarray(0, 22 + 2 * SEALED + 1000)));
  const offset = decryptor.resumeAt();
  assert.equal(offset, 22 + 2 * SEALED, 'chunks 0 and 1 were opened; the third is partial');
  assert.equal(got.length, 2);
  got.push(...await decryptor.push(cipher.subarray(offset)));
  got.push(...await decryptor.finish());
  assert.ok(Buffer.concat(got).equals(plain));
});

test('the file name rides inside the stream and is cleaned for saving', () => {
  const name = Buffer.from('relatório final.pdf');
  const stream = Buffer.concat([Buffer.from([0, name.length]), name, Buffer.from('DATA')]);
  const splitter = page.createSplitter();
  const out = [];
  for (const byte of stream) out.push(Buffer.from(splitter.feed(Buffer.from([byte]))));
  assert.equal(splitter.name(), 'relatório final.pdf');
  assert.equal(Buffer.concat(out).toString(), 'DATA');
  assert.throws(() => page.createSplitter().feed(Buffer.from([0, 0, 1])), page.ShareError);
  assert.throws(() => page.createSplitter().feed(Buffer.from([0xff, 0xff, 1])), page.ShareError);
  assert.throws(() => page.createSplitter().feed(Buffer.from([0, 2, 0xff, 0xfe])), page.ShareError);

  assert.equal(page.safeName('..\\..\\evil/name.exe'), '.._.._evil_name.exe'.replace(/^[. ]+/, ''));
  assert.equal(page.safeName('con:?.txt'), 'con__.txt');
  assert.equal(page.safeName('..'), 'download');
  assert.equal(page.safeName('a‮b.txt'), 'a_b.txt');
  assert.ok(page.safeName('x'.repeat(500)).length <= 200);
});

test('the fragment key must be exactly 32 bytes of base64url', () => {
  const good = Buffer.alloc(32, 7).toString('base64url');
  assert.equal(good.length, 43);
  assert.deepEqual([...page.keyFromFragment(good)], [...Buffer.alloc(32, 7)]);
  for (const bad of ['', good.slice(1), `${good}A`, `${good.slice(0, 42)}=`, 'é'.repeat(43), `+${good.slice(1)}`]) {
    assert.throws(() => page.keyFromFragment(bad), damaged('key'), bad);
  }
});
