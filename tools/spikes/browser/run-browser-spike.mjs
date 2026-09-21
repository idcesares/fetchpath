import { createServer } from "node:http";
import { spawn } from "node:child_process";
import { mkdir, readFile, rm, writeFile } from "node:fs/promises";
import path from "node:path";
import process from "node:process";

const EXTENSION_ID = "lfikhkjdpjcjaboanknaabncpkbgoele";
const CASES = [
  ["get", true, null],
  ["signed", true, null],
  ["auth", false, "browser_auth_context_required"],
  ["post", false, "unsupported_method"],
  ["blob", false, "unsupported_scheme"],
];

const [chromePath, extensionDir, profileDir, downloadDir, ledgerPath, evidencePath] = process.argv.slice(2);
if (![chromePath, extensionDir, profileDir, downloadDir, ledgerPath, evidencePath].every(Boolean)) {
  throw new Error("usage: run-browser-spike.mjs <chrome> <extension-dir> <profile-dir> <downloads-dir> <ledger> <evidence>");
}

const delay = (milliseconds) => new Promise((resolve) => setTimeout(resolve, milliseconds));

async function waitFor(description, operation, timeoutMs = 15_000) {
  const deadline = Date.now() + timeoutMs;
  let lastError;
  while (Date.now() < deadline) {
    try {
      const result = await operation();
      if (result) return result;
    } catch (error) {
      lastError = error;
    }
    await delay(100);
  }
  throw new Error(`timed out waiting for ${description}${lastError ? `: ${lastError.message}` : ""}`);
}

class CdpClient {
  constructor(socket) {
    this.socket = socket;
    this.nextId = 1;
    this.pending = new Map();
    socket.addEventListener("message", (event) => {
      const message = JSON.parse(event.data);
      if (!message.id) return;
      const pending = this.pending.get(message.id);
      if (!pending) return;
      this.pending.delete(message.id);
      if (message.error) pending.reject(new Error(message.error.message));
      else pending.resolve(message.result);
    });
    socket.addEventListener("close", () => {
      for (const pending of this.pending.values()) pending.reject(new Error("CDP connection closed"));
      this.pending.clear();
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

  close() {
    this.socket.close();
  }
}

function slowDownload(response, label) {
  const chunk = Buffer.alloc(64 * 1024, label.charCodeAt(0));
  let sent = 0;
  const total = 4 * 1024 * 1024;
  response.writeHead(200, {
    "content-type": "application/octet-stream",
    "content-disposition": `attachment; filename="${label}.bin"`,
    "content-length": total,
    "cache-control": "no-store",
  });
  const timer = setInterval(() => {
    if (sent >= total) {
      clearInterval(timer);
      response.end();
      return;
    }
    response.write(chunk);
    sent += chunk.length;
  }, 20);
  response.on("close", () => clearInterval(timer));
}

function casesHtml() {
  return `<!doctype html>
<html lang="en"><meta charset="utf-8"><title>Fetchpath fixture</title><body>
<script>
function clickDownload(url, filename) {
  const link = document.createElement("a");
  link.href = url;
  link.download = filename;
  document.body.append(link);
  link.click();
  link.remove();
}
window.runCase = async (name) => {
  if (name === "get") clickDownload("/download/get.bin", "get.bin");
  if (name === "signed") clickDownload("/download/signed.bin?token=signed-secret-value&expires=4102444800", "signed.bin");
  if (name === "auth") {
    await fetch("/set-cookie", { credentials: "include" });
    clickDownload("/download/auth.bin", "auth.bin");
  }
  if (name === "post") {
    const form = document.createElement("form");
    form.method = "POST";
    form.action = "/download/post.bin";
    const input = document.createElement("input");
    input.name = "csrf";
    input.value = "post-secret-value";
    form.append(input);
    document.body.append(form);
    form.submit();
  }
  if (name === "blob") {
    const blobUrl = URL.createObjectURL(new Blob(["blob fixture"], { type: "application/octet-stream" }));
    clickDownload(blobUrl, "blob.bin");
    setTimeout(() => URL.revokeObjectURL(blobUrl), 5000);
  }
  return name;
};
</script></body></html>`;
}

function startFixtureServer() {
  const server = createServer((request, response) => {
    const url = new URL(request.url, "http://127.0.0.1");
    if (url.pathname === "/cases.html") {
      response.writeHead(200, { "content-type": "text/html; charset=utf-8", "cache-control": "no-store" });
      response.end(casesHtml());
      return;
    }
    if (url.pathname === "/set-cookie") {
      response.writeHead(204, { "set-cookie": "session=valid; Path=/; HttpOnly; SameSite=Strict" });
      response.end();
      return;
    }
    if (url.pathname === "/download/auth.bin" && !/(?:^|;\s*)session=valid(?:;|$)/.test(request.headers.cookie ?? "")) {
      response.writeHead(401, { "content-type": "text/plain" });
      response.end("authentication required");
      return;
    }
    if (url.pathname.startsWith("/download/")) {
      slowDownload(response, path.basename(url.pathname, ".bin"));
      return;
    }
    response.writeHead(404).end();
  });
  return new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", () => {
      const address = server.address();
      resolve({ server, baseUrl: `http://127.0.0.1:${address.port}` });
    });
  });
}

async function readDevtoolsPort() {
  const text = await readFile(path.join(profileDir, "DevToolsActivePort"), "utf8");
  return Number(text.split(/\r?\n/, 1)[0]);
}

async function listTargets(port) {
  const response = await fetch(`http://127.0.0.1:${port}/json/list`);
  if (!response.ok) throw new Error(`target list failed: ${response.status}`);
  return response.json();
}

async function evaluate(client, expression, userGesture = false) {
  const result = await client.send("Runtime.evaluate", {
    expression,
    awaitPromise: true,
    returnByValue: true,
    userGesture,
  });
  if (result.exceptionDetails) {
    throw new Error(result.exceptionDetails.exception?.description ?? result.exceptionDetails.text);
  }
  return result.result.value;
}

async function main() {
  await rm(profileDir, { recursive: true, force: true });
  await rm(downloadDir, { recursive: true, force: true });
  await rm(ledgerPath, { force: true });
  await mkdir(profileDir, { recursive: true });
  await mkdir(downloadDir, { recursive: true });
  await mkdir(path.dirname(evidencePath), { recursive: true });

  const { server, baseUrl } = await startFixtureServer();
  const extensionUrl = `chrome-extension://${EXTENSION_ID}/probe.html`;
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
    "--disable-popup-blocking",
    extensionUrl,
  ], {
    stdio: ["ignore", "pipe", "pipe"],
    env: { ...process.env, FETCHPATH_BROWSER_SPIKE_LEDGER: ledgerPath },
  });

  let chromeStderr = "";
  chrome.stderr.on("data", (chunk) => {
    chromeStderr = `${chromeStderr}${chunk}`.slice(-16_384);
  });

  let browserClient;
  let probeClient;
  try {
    const port = await waitFor("Chrome DevTools port", readDevtoolsPort, 20_000);
    const version = await waitFor("Chrome DevTools endpoint", async () => {
      const response = await fetch(`http://127.0.0.1:${port}/json/version`);
      return response.ok ? response.json() : null;
    });
    browserClient = await CdpClient.connect(version.webSocketDebuggerUrl);
    await browserClient.send("Browser.setDownloadBehavior", {
      behavior: "allow",
      downloadPath: downloadDir,
      eventsEnabled: true,
    });

    const probeTarget = await waitFor("extension probe page", async () =>
      (await listTargets(port)).find((target) => target.url.startsWith(extensionUrl)), 20_000);
    probeClient = await CdpClient.connect(probeTarget.webSocketDebuggerUrl);
    await probeClient.send("Runtime.enable");

    const nativeProbe = await waitFor("native host acknowledgement", async () => {
      const result = await evaluate(probeClient,
        "chrome.runtime.sendMessage({type:'probe'}).catch(error => ({error:error.message}))");
      if (result?.error) throw new Error(result.error);
      if (result?.accepted === false) throw new Error(result.reason ?? "native host rejected probe");
      return result?.accepted === true ? result : null;
    }, 15_000);
    await evaluate(probeClient, "chrome.runtime.sendMessage({type:'clear_events'})");

    const caseResults = [];
    for (const [caseName, expectedAccepted, expectedReason] of CASES) {
      const before = await evaluate(probeClient, "chrome.runtime.sendMessage({type:'get_events'})");
      const created = await browserClient.send("Target.createTarget", { url: `${baseUrl}/cases.html` });
      const caseTarget = await waitFor(`${caseName} fixture tab`, async () =>
        (await listTargets(port)).find((target) => target.id === created.targetId));
      const caseClient = await CdpClient.connect(caseTarget.webSocketDebuggerUrl);
      await caseClient.send("Runtime.enable");
      await caseClient.send("Page.enable");
      await waitFor(`${caseName} fixture script`, async () =>
        (await evaluate(caseClient, "typeof window.runCase === 'function'")) === true);
      await evaluate(caseClient, `window.runCase(${JSON.stringify(caseName)})`, true);
      let event;
      try {
        event = await waitFor(`${caseName} handoff result`, async () => {
          const events = await evaluate(probeClient, "chrome.runtime.sendMessage({type:'get_events'})");
          return events.length > before.length ? events.at(-1) : null;
        }, 15_000);
      } catch (error) {
        const diagnostic = await evaluate(probeClient,
          "Promise.all([chrome.runtime.sendMessage({type:'get_events'}), chrome.downloads.search({})])");
        throw new Error(`${error.message}; diagnostic=${JSON.stringify(diagnostic)}`);
      }
      caseClient.close();
      await browserClient.send("Target.closeTarget", { targetId: created.targetId });

      const passed = event.accepted === expectedAccepted
        && (expectedAccepted ? event.cancel_observed === true : event.fallback === true)
        && (expectedReason === null || event.reason === expectedReason);
      caseResults.push({ case: caseName, expectedAccepted, expectedReason, passed, event });
    }

    const duplicateRequest = {
      schema_version: 1,
      type: "capture",
      capture_id: "duplicate-probe",
      method: "GET",
      url: `${baseUrl}/download/duplicate.bin?token=duplicate-secret-value`,
      has_auth_context: false,
      observation_complete: true,
      signed_hint: true,
    };
    const duplicateFirst = await evaluate(probeClient,
      `chrome.runtime.sendMessage({type:'direct_capture_for_test',request:${JSON.stringify(duplicateRequest)}})`);
    const duplicateSecond = await evaluate(probeClient,
      `chrome.runtime.sendMessage({type:'direct_capture_for_test',request:${JSON.stringify(duplicateRequest)}})`);
    const duplicateCheck = {
      first: duplicateFirst,
      second: duplicateSecond,
      passed: duplicateFirst?.accepted === true
        && duplicateFirst?.deduplicated === false
        && duplicateSecond?.accepted === true
        && duplicateSecond?.deduplicated === true,
    };

    const ledgerText = await readFile(ledgerPath, "utf8");
    const ledgerRecords = ledgerText.trim().split(/\r?\n/).filter(Boolean).map((line) => JSON.parse(line));
    const secretMarkers = ["signed-secret-value", "duplicate-secret-value", "post-secret-value", "session=valid"];
    const secretPersistenceCheck = secretMarkers.every((marker) => !ledgerText.includes(marker));
    const passed = nativeProbe.accepted === true
      && caseResults.every((result) => result.passed)
      && duplicateCheck.passed
      && ledgerRecords.length === 3
      && secretPersistenceCheck;
    const evidence = {
      schemaVersion: 1,
      task: "FP-006",
      runAt: new Date().toISOString(),
      platform: `${process.platform} ${process.arch}`,
      browser: {
        name: process.env.FETCHPATH_BROWSER_CHANNEL ?? "Chrome for Testing",
        fileVersion: process.env.FETCHPATH_BROWSER_VERSION ?? "unknown",
        product: version.Browser,
        source: process.env.FETCHPATH_BROWSER_SOURCE ?? "unknown",
        isolatedProfile: true,
      },
      nativeMessaging: {
        api: "chrome.runtime.sendNativeMessage",
        probe: nativeProbe,
        durableAckLedgerRecords: ledgerRecords,
      },
      cases: caseResults,
      duplicateCheck,
      secretPersistenceCheck,
      overall: passed ? "pass" : "fail",
    };
    await writeFile(evidencePath, `${JSON.stringify(evidence, null, 2)}\n`, "utf8");
    if (!passed) throw new Error(`browser spike assertions failed; see ${evidencePath}`);
    process.stdout.write(`${JSON.stringify(evidence, null, 2)}\n`);
  } finally {
    probeClient?.close();
    browserClient?.close();
    chrome.kill();
    await new Promise((resolve) => server.close(resolve));
    await Promise.race([
      new Promise((resolve) => chrome.once("exit", resolve)),
      delay(3000),
    ]);
    if (chrome.exitCode === null) chrome.kill("SIGKILL");
    if (process.exitCode && chromeStderr) process.stderr.write(chromeStderr);
  }
}

await main();
