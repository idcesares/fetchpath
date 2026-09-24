// Throttled HTTP fixture: strong ETag, byte ranges, request log.
import http from 'node:http';
import crypto from 'node:crypto';
import fs from 'node:fs';

const port = Number(process.argv[2] ?? 18470);
const log = process.argv[3];
function body(size, seed) {
  const out = Buffer.alloc(size);
  let x = seed;
  for (let i = 0; i < size; i++) { x = (x * 1103515245 + 12345) & 0x7fffffff; out[i] = x & 0xff; }
  return out;
}
const files = {
  '/small.bin': { data: body(256 * 1024, 7), rate: Infinity },
  '/big.bin': { data: body(12 * 1024 * 1024, 11), rate: 1024 * 1024 },
  '/second.bin': { data: body(8 * 1024 * 1024, 13), rate: 1024 * 1024 },
};
for (const file of Object.values(files)) {
  file.sha = crypto.createHash('sha256').update(file.data).digest('hex');
  file.etag = `"${file.sha.slice(0, 16)}"`;
}
fs.writeFileSync(log + '.hashes.json', JSON.stringify(Object.fromEntries(Object.entries(files).map(([k, v]) => [k, v.sha]))));

http.createServer((req, res) => {
  const file = files[req.url];
  fs.appendFileSync(log, JSON.stringify({ t: Date.now(), m: req.method, u: req.url, range: req.headers.range ?? null, ifRange: req.headers['if-range'] ?? null }) + '\n');
  if (!file) { res.writeHead(404); return res.end(); }
  let start = 0, end = file.data.length - 1, status = 200;
  const range = /^bytes=(\d+)-(\d*)$/.exec(req.headers.range ?? '');
  const ifRangeOk = !req.headers['if-range'] || req.headers['if-range'] === file.etag;
  if (range && ifRangeOk) {
    start = Number(range[1]); if (range[2]) end = Math.min(end, Number(range[2]));
    status = 206;
  }
  const headers = { 'Content-Type': 'application/octet-stream', 'Accept-Ranges': 'bytes', ETag: file.etag, 'Content-Length': end - start + 1 };
  if (status === 206) headers['Content-Range'] = `bytes ${start}-${end}/${file.data.length}`;
  res.writeHead(status, headers);
  if (req.method === 'HEAD') return res.end();
  let offset = start;
  const chunk = 64 * 1024;
  const tick = () => {
    if (res.destroyed) return;
    if (offset > end) return res.end();
    const next = Math.min(end + 1, offset + chunk);
    res.write(file.data.subarray(offset, next));
    offset = next;
    if (file.rate === Infinity) setImmediate(tick); else setTimeout(tick, (chunk / file.rate) * 1000);
  };
  tick();
}).listen(port, '127.0.0.1', () => console.log(`listening ${port}`));
