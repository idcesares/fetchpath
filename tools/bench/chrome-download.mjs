import { spawn } from 'node:child_process';
import { mkdtemp, mkdir, readFile, rename, rm } from 'node:fs/promises';
import { join, resolve, dirname } from 'node:path';

export const CHROME_SETTINGS = {
  mode: 'native download via Page.navigate and Browser.downloadProgress completed',
  isolation: 'new profile, disk cache and download directory for every run; extensions disabled',
  timing: 'cold browser spawn through native download completion and browser shutdown; independent SHA-256 follows',
  cache: 'Network.setCacheDisabled plus a fresh disk cache for each run',
  proxy: 'disabled',
  cpuAndMemory: 'not collected: Chrome uses multiple subprocesses',
};

class CdpClient {
  constructor(socket, onEvent) {
    this.socket = socket; this.pending = new Map(); this.nextId = 1;
    socket.addEventListener('message', event => {
      const message = JSON.parse(event.data);
      const pending = this.pending.get(message.id);
      if (!pending) { onEvent(message); return; }
      this.pending.delete(message.id);
      if (message.error) pending.reject(new Error(message.error.message)); else pending.resolve(message.result);
    });
    socket.addEventListener('close', () => {
      for (const pending of this.pending.values()) pending.reject(new Error('Chrome DevTools connection closed'));
      this.pending.clear();
    });
  }
  static async connect(url, onEvent) {
    const socket = new WebSocket(url);
    await new Promise((resolveOpen, reject) => {
      socket.addEventListener('open', resolveOpen, { once: true });
      socket.addEventListener('error', reject, { once: true });
    });
    return new CdpClient(socket, onEvent);
  }
  send(method, params = {}, sessionId) {
    const id = this.nextId++;
    return new Promise((resolveSend, reject) => {
      this.pending.set(id, { resolve: resolveSend, reject });
      this.socket.send(JSON.stringify({ id, method, params, sessionId }));
    });
  }
}

export async function measureChrome(command, url, file, { timeoutMs }) {
  // Only recursively remove this newly allocated child directory, never a caller-provided path.
  const scratch = await mkdtemp(join(dirname(resolve(file)), 'chrome-run-'));
  const profile = join(scratch, 'profile'); const downloads = join(scratch, 'downloads');
  await mkdir(downloads); await mkdir(profile);
  let child; let cdp; let timer; let timedOut = false; let version; let transfer;
  let exitPromise; let errorMessage; let code = null; let wallMs;
  const started = process.hrtime.bigint();
  try {
    child = spawn(command, ['--headless=new', `--user-data-dir=${profile}`, `--disk-cache-dir=${join(scratch, 'cache')}`,
      '--remote-debugging-address=127.0.0.1', '--remote-debugging-port=0', '--disable-extensions', '--no-first-run',
      '--no-default-browser-check', '--disable-background-networking', '--disable-component-update', '--no-proxy-server', 'about:blank'],
    { windowsHide: true, stdio: ['ignore', 'ignore', 'pipe'] });
    let exited = false;
    exitPromise = new Promise(resolveExit => {
      child.once('error', error => { errorMessage = error.message; exited = true; resolveExit(null); });
      child.once('exit', exitCode => { exited = true; resolveExit(exitCode); });
    });
    child.stderr.on('data', () => {}); // Drain Chrome diagnostics without retaining machine paths or URLs.
    const operation = (async () => {
      let portFile;
      while (!portFile) {
        if (exited) throw new Error(errorMessage ?? 'Chrome exited before DevTools became available');
        try { portFile = await readFile(join(profile, 'DevToolsActivePort'), 'utf8'); } catch { await new Promise(res => setTimeout(res, 25)); }
      }
      const [port, endpoint] = portFile.trim().split(/\r?\n/);
      let resolveDownload; let rejectDownload; let guid;
      const completed = new Promise((res, rej) => { resolveDownload = res; rejectDownload = rej; });
      // Completion can reject before navigation returns; attach a handler immediately.
      completed.catch(() => {});
      cdp = await CdpClient.connect(`ws://127.0.0.1:${port}${endpoint}`, event => {
        if (event.method === 'Browser.downloadWillBegin') guid = event.params.guid;
        if (event.method !== 'Browser.downloadProgress' || event.params.guid !== guid) return;
        if (event.params.state === 'completed') resolveDownload(event.params);
        if (event.params.state === 'canceled') rejectDownload(new Error('Chrome native download canceled'));
      });
      version = (await cdp.send('Browser.getVersion')).product;
      await cdp.send('Browser.setDownloadBehavior', { behavior: 'allowAndName', downloadPath: downloads, eventsEnabled: true });
      const { targetId } = await cdp.send('Target.createTarget', { url: 'about:blank' });
      const { sessionId } = await cdp.send('Target.attachToTarget', { targetId, flatten: true });
      await cdp.send('Network.enable', {}, sessionId);
      await cdp.send('Network.setCacheDisabled', { cacheDisabled: true }, sessionId);
      const navigationStarted = process.hrtime.bigint();
      const startupMs = Number(navigationStarted - started) / 1e6;
      const navigation = await cdp.send('Page.navigate', { url }, sessionId);
      if (navigation.errorText && navigation.errorText !== 'net::ERR_ABORTED') throw new Error(`Chrome navigation failed: ${navigation.errorText}`);
      const progress = await completed;
      const nativeDownloadMs = Number(process.hrtime.bigint() - navigationStarted) / 1e6;
      // allowAndName stores the actual browser-downloaded file under its GUID.
      await rename(join(downloads, progress.guid), file);
      transfer = { browserVersion: version, completed: true, startupMs, nativeDownloadMs, receivedBytes: progress.receivedBytes, totalBytes: progress.totalBytes };
      await cdp.send('Browser.close');
      code = await exitPromise;
      if (code !== 0) throw new Error(`Chrome exited with code ${code}`);
    })();
    const timeout = new Promise((_, reject) => { timer = setTimeout(() => { timedOut = true; reject(new Error('Chrome native download timed out (response may not support native download)')); }, timeoutMs); });
    await Promise.race([operation, timeout]);
  } catch (error) {
    errorMessage = error.message;
    code = null;
  } finally {
    clearTimeout(timer);
    if (child && child.exitCode === null) {
      // Browser.close affects only our isolated instance; killing our own launcher is the fallback.
      if (cdp) await Promise.race([cdp.send('Browser.close').catch(() => {}), new Promise(res => setTimeout(res, 1000))]);
      if (exitPromise) await Promise.race([exitPromise, new Promise(res => setTimeout(res, 1000))]);
      if (child.exitCode === null) child.kill();
      if (exitPromise) await exitPromise;
    }
    cdp?.socket.close();
    wallMs = Number(process.hrtime.bigint() - started) / 1e6;
    await rm(scratch, { recursive: true, force: true, maxRetries: 5, retryDelay: 100 });
  }
  return { code, stdout: JSON.stringify(transfer ?? { browserVersion: version, completed: false }), stderr: errorMessage ?? '',
    spawnError: errorMessage, timedOut, observed: null, wallMs };
}
