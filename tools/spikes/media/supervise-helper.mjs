import { createReadStream, existsSync, promises as fs } from 'node:fs';
import http from 'node:http';
import path from 'node:path';
import { spawn } from 'node:child_process';

const [ytDlp, ffmpegDir, fixtures, output, evidenceFile] = process.argv.slice(2);
if (![ytDlp, ffmpegDir, fixtures, output, evidenceFile].every(Boolean)) throw new Error('Usage: supervise-helper.mjs <yt-dlp> <ffmpeg-dir> <fixtures> <output> <evidence.jsonl>');
const events = [];
const emit = (event) => events.push({ at: new Date().toISOString(), ...event });
const contentType = (file) => file.endsWith('.mpd') ? 'application/dash+xml' : file.endsWith('.m3u8') ? 'application/vnd.apple.mpegurl' : file.endsWith('.m4s') ? 'video/iso.segment' : file.endsWith('.ts') ? 'video/mp2t' : 'video/mp4';
const server = http.createServer(async (request, response) => {
  const pathname = new URL(request.url, 'http://127.0.0.1').pathname;
  const requested = decodeURIComponent(pathname).replace(/^\/+/, '');
  const file = path.resolve(fixtures, requested);
  if (!file.startsWith(path.resolve(fixtures) + path.sep) || !existsSync(file)) { response.writeHead(404, { 'content-type': 'text/plain' }); response.end('fixture not found'); return; }
  // `direct.mp4` exists solely for the cancellation case and is intentionally
  // throttled; HLS and DASH segment requests remain fast for normal completion.
  const slow = requested === 'direct.mp4';
  response.writeHead(200, { 'content-type': contentType(file), 'content-length': String((await fs.stat(file)).size), 'accept-ranges': 'bytes' });
  if (!slow) { createReadStream(file).pipe(response); return; }
  const stream = createReadStream(file, { highWaterMark: 8192 });
  stream.on('data', (chunk) => { stream.pause(); response.write(chunk); setTimeout(() => stream.resume(), 40); });
  stream.on('end', () => response.end());
});

function run(operation, args, cancelAfterMs) {
  return new Promise((resolve) => {
    emit({ event: 'start', operation, args });
    const child = spawn(ytDlp, ['--ignore-config', '--no-playlist', '--newline', '--ffmpeg-location', ffmpegDir, '--progress-template', 'download:FETCHPATH_PROGRESS|%(progress._percent_str)s|%(progress._speed_str)s|%(progress._eta_str)s', ...args], { windowsHide: true });
    let cancelled = false;
    let cancelTimer;
    const requestCancellation = () => {
      cancelled = true;
      emit({ event: 'cancel-requested', operation, pid: child.pid });
      // Windows requires terminating the helper process tree, not merely sending
      // a POSIX-style signal to the immediate process.
      spawn('taskkill', ['/pid', String(child.pid), '/t', '/f'], { windowsHide: true });
    };
    const consume = (stream, channel) => stream.setEncoding('utf8').on('data', (chunk) => chunk.split(/\r?\n/).filter(Boolean).forEach((line) => {
      if (line.startsWith('FETCHPATH_PROGRESS|')) {
        const [, percent, speed, eta] = line.split('|');
        emit({ event: 'progress', operation, percent, speed, eta });
        if (cancelAfterMs && !cancelTimer) cancelTimer = setTimeout(requestCancellation, cancelAfterMs);
      }
      else emit({ event: 'helper-output', operation, channel, line });
    }));
    consume(child.stdout, 'stdout'); consume(child.stderr, 'stderr');
    child.on('close', (code, signal) => { if (cancelTimer) clearTimeout(cancelTimer); emit({ event: 'exit', operation, code, signal, cancelled }); resolve({ code, cancelled }); });
    child.on('error', (error) => { if (cancelTimer) clearTimeout(cancelTimer); emit({ event: 'spawn-error', operation, message: error.message }); resolve({ code: -1, cancelled }); });
  });
}

server.listen(0, '127.0.0.1', async () => {
  const { port } = server.address();
  const url = (part) => `http://127.0.0.1:${port}/${part}`;
  const result = {};
  result.enumerateDash = await run('enumerate-dash', ['-J', url('dash/manifest.mpd')]);
  result.muxDash = await run('mux-dash', ['-f', 'bestvideo+bestaudio', '--merge-output-format', 'mp4', '-o', path.join(output, 'dash-muxed.%(ext)s'), url('dash/manifest.mpd')]);
  result.audioDash = await run('audio-dash', ['-f', 'bestaudio', '-x', '--audio-format', 'mp3', '-o', path.join(output, 'dash-audio.%(ext)s'), url('dash/manifest.mpd')]);
  result.hls = await run('hls-muxed', ['-o', path.join(output, 'hls.%(ext)s'), url('hls/master.m3u8')]);
  result.cancel = await run('cancel-direct', ['-o', path.join(output, 'cancelled.%(ext)s'), url('direct.mp4')], 700);
  result.failure = await run('missing-fixture', ['-o', path.join(output, 'missing.%(ext)s'), url('missing.m3u8')]);
  emit({ event: 'summary', result });
  await fs.writeFile(evidenceFile, events.map((event) => JSON.stringify(event)).join('\n') + '\n');
  server.close(() => process.exit((result.enumerateDash.code === 0 && result.muxDash.code === 0 && result.audioDash.code === 0 && result.hls.code === 0 && result.cancel.cancelled && result.failure.code !== 0) ? 0 : 1));
});
