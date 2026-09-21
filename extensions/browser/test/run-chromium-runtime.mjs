import { createServer } from "node:http";
import { spawn } from "node:child_process";
import { cp, mkdir, readFile, readdir, rm, writeFile } from "node:fs/promises";
import path from "node:path";

const EXTENSION_ID = "lfikhkjdpjcjaboanknaabncpkbgoele";
const [chromePath, sourceDir, workDir, evidencePath] = process.argv.slice(2);
if (![chromePath, sourceDir, workDir, evidencePath].every(Boolean)) {
  throw new Error("usage: run-chromium-runtime.mjs <chrome> <source-extension> <work-dir> <evidence>");
}
const delay = (milliseconds) => new Promise((resolve) => setTimeout(resolve, milliseconds));

async function waitFor(label, operation, timeoutMs = 20_000) {
  const deadline = Date.now() + timeoutMs;
  let lastError;
  while (Date.now() < deadline) {
    try {
      const value = await operation();
      if (value) return value;
    } catch (error) {
      lastError = error;
    }
    await delay(100);
  }
  throw new Error(`timed out waiting for ${label}${lastError ? `: ${lastError.message}` : ""}`);
}

class CdpClient {
  constructor(socket) {
    this.socket = socket;
    this.nextId = 1;
    this.pending = new Map();
    socket.addEventListener("message", (event) => {
      const message = JSON.parse(event.data);
      const pending = this.pending.get(message.id);
      if (!pending) return;
      this.pending.delete(message.id);
      if (message.error) pending.reject(new Error(message.error.message));
      else pending.resolve(message.result);
    });
  }
  static async connect(url) {
    const socket = new WebSocket(url);
    await new Promise((resolve, reject) => {
      socket.addEventListener("open", resolve, { once: true });
      socket.addEventListener("error", reject, { once: true });
    });
    return new CdpClient(socket);
  }
  send(method, params = {}) {
    const id = this.nextId++;
    return new Promise((resolve, reject) => {
      this.pending.set(id, { resolve, reject });
      this.socket.send(JSON.stringify({ id, method, params }));
    });
  }
  close() { this.socket.close(); }
}

async function evaluate(client, expression) {
  const result = await client.send("Runtime.evaluate", {
    expression,
    awaitPromise: true,
    returnByValue: true,
    userGesture: true,
  });
  if (result.exceptionDetails) throw new Error(result.exceptionDetails.exception?.description ?? result.exceptionDetails.text);
  return result.result.value;
}

function startFixture() {
  const server = createServer((request, response) => {
    const url = new URL(request.url, "http://127.0.0.1");
    if (url.pathname === "/set-cookie") {
      response.writeHead(200, {
        "content-type": "text/html",
        "set-cookie": "session=runtime-secret; Path=/; HttpOnly; SameSite=Lax",
      });
      response.end("cookie set");
      return;
    }
    if (url.pathname === "/archive.bin") {
      response.writeHead(200, { "content-type": "application/octet-stream", "content-length": 7 });
      response.end("fixture");
      return;
    }
    response.writeHead(404).end();
  });
  return new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", () => resolve({
      server,
      baseUrl: `http://127.0.0.1:${server.address().port}`,
    }));
  });
}

async function main() {
  const extensionDir = path.join(workDir, "extension");
  const profileDir = path.join(workDir, "profile");
  const appDataDir = path.join(workDir, "app-data");
  await rm(workDir, { recursive: true, force: true });
  await mkdir(extensionDir, { recursive: true });
  await mkdir(profileDir, { recursive: true });
  await mkdir(appDataDir, { recursive: true });
  for (const filename of ["background.js", "policy.js"]) {
    await cp(path.join(sourceDir, filename), path.join(extensionDir, filename));
  }
  await mkdir(path.join(extensionDir, "test", "runtime"), { recursive: true });
  await cp(path.join(sourceDir, "test", "runtime", "probe.html"), path.join(extensionDir, "test", "runtime", "probe.html"));
  const manifest = JSON.parse(await readFile(path.join(sourceDir, "manifest.chromium.json"), "utf8"));
  manifest.host_permissions = ["http://127.0.0.1/*"];
  await writeFile(path.join(extensionDir, "manifest.json"), `${JSON.stringify(manifest, null, 2)}\n`);

  const { server, baseUrl } = await startFixture();
  const extensionUrl = `chrome-extension://${EXTENSION_ID}/test/runtime/probe.html`;
  const chrome = spawn(chromePath, [
    "--headless=new",
    `--user-data-dir=${profileDir}`,
    `--load-extension=${extensionDir}`,
    `--disable-extensions-except=${extensionDir}`,
    "--remote-debugging-port=0",
    "--no-first-run",
    "--no-default-browser-check",
    "--disable-background-networking",
    "--disable-component-update",
    extensionUrl,
  ], {
    stdio: ["ignore", "ignore", "pipe"],
    env: { ...process.env, FETCHPATH_APP_DATA_DIR: appDataDir },
  });
  let browserClient;
  let probeClient;
  try {
    const port = await waitFor("DevTools port", async () => {
      const text = await readFile(path.join(profileDir, "DevToolsActivePort"), "utf8");
      return Number(text.split(/\r?\n/, 1)[0]);
    });
    const version = await waitFor("DevTools endpoint", async () => {
      const response = await fetch(`http://127.0.0.1:${port}/json/version`);
      return response.ok ? response.json() : null;
    });
    browserClient = await CdpClient.connect(version.webSocketDebuggerUrl);
    const targets = () => fetch(`http://127.0.0.1:${port}/json/list`).then((response) => response.json());
    const cookieTarget = await browserClient.send("Target.createTarget", { url: `${baseUrl}/set-cookie` });
    await waitFor("cookie fixture", async () => (await targets()).find((target) => target.id === cookieTarget.targetId && target.url.includes("set-cookie")));
    await delay(300);

    const probeTarget = await waitFor("extension page", async () => (await targets()).find((target) => target.url === extensionUrl));
    probeClient = await CdpClient.connect(probeTarget.webSocketDebuggerUrl);
    await probeClient.send("Runtime.enable");
    const captureUrl = `${baseUrl}/archive.bin?token=runtime-signed-secret`;
    const accepted = await evaluate(probeClient, `chrome.runtime.sendMessage({type:'send_link',url:${JSON.stringify(captureUrl)}})`);
    if (accepted?.accepted !== true) throw new Error(`capture rejected: ${JSON.stringify(accepted)}`);
    const state = await evaluate(probeClient, "chrome.runtime.sendMessage({type:'get_state'})");
    if (state.events.at(-1)?.cookieCount !== 1) throw new Error(`cookie capture missing: ${JSON.stringify(state)}`);
    await evaluate(probeClient, `chrome.runtime.sendMessage({type:'set_excluded',url:${JSON.stringify(baseUrl)},excluded:true})`);
    const excluded = await evaluate(probeClient, `chrome.runtime.sendMessage({type:'send_link',url:${JSON.stringify(captureUrl)}})`);
    if (excluded?.reason !== "site_excluded") throw new Error(`exclusion failed: ${JSON.stringify(excluded)}`);

    const inboxFiles = await readdir(path.join(appDataDir, "browser-inbox"));
    if (inboxFiles.length !== 1) throw new Error(`expected one inbox record, got ${inboxFiles.length}`);
    const inboxText = await readFile(path.join(appDataDir, "browser-inbox", inboxFiles[0]), "utf8");
    if (/runtime-signed-secret|runtime-secret|session/.test(inboxText)) throw new Error("public inbox leaked a secret");
    const eventText = JSON.stringify(state);
    if (/runtime-signed-secret|runtime-secret|session/.test(eventText)) throw new Error("extension storage leaked a secret");

    const evidence = {
      schemaVersion: 1,
      task: "FP-013",
      runAt: new Date().toISOString(),
      browser: version.Browser,
      isolatedProfile: true,
      explicitCaptureAccepted: true,
      httpOnlyCookieCount: 1,
      exclusionPreservedBrowserPath: true,
      durableInboxRecords: 1,
      publicSecretLeak: false,
      overall: "pass",
    };
    await mkdir(path.dirname(evidencePath), { recursive: true });
    await writeFile(evidencePath, `${JSON.stringify(evidence, null, 2)}\n`);
    process.stdout.write(`${JSON.stringify(evidence, null, 2)}\n`);
  } finally {
    probeClient?.close();
    browserClient?.close();
    chrome.kill();
    await new Promise((resolve) => server.close(resolve));
    await Promise.race([new Promise((resolve) => chrome.once("exit", resolve)), delay(3000)]);
    if (chrome.exitCode === null) chrome.kill("SIGKILL");
  }
}

await main();
