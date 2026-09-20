import http from 'node:http';
import { readFile } from 'node:fs/promises';

const assets = new Map([
  ['/', ['index.html', 'text/html; charset=utf-8']],
  ['/index.html', ['index.html', 'text/html; charset=utf-8']],
  ['/styles.css', ['styles.css', 'text/css; charset=utf-8']],
  ['/app.js', ['app.js', 'text/javascript; charset=utf-8']],
]);
const server = http.createServer(async (request, response) => {
  if (!['GET', 'HEAD'].includes(request.method)) { response.writeHead(405).end(); return; }
  const asset = assets.get(request.url?.split('?')[0]);
  if (!asset) { response.writeHead(404).end('Not found'); return; }
  try {
    const body = await readFile(new URL(`../prototypes/desktop/${asset[0]}`, import.meta.url));
    response.writeHead(200, { 'Content-Type': asset[1], 'Cache-Control': 'no-store', 'X-Content-Type-Options': 'nosniff' });
    response.end(request.method === 'HEAD' ? undefined : body);
  } catch { response.writeHead(500).end('Prototype asset unavailable'); }
});
server.on('error', error => { console.error(error.message); process.exitCode = 1; });
server.listen(4173, '127.0.0.1', () => console.log('Fetchpath simulated UX prototype: http://127.0.0.1:4173'));
for (const signal of ['SIGINT', 'SIGTERM']) process.on(signal, () => server.close());
