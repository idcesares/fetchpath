import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { spawn } from "node:child_process";
import test from "node:test";

const hostPath = resolve("target/debug/fetchpath-browser-host.exe");
const caller = "chrome-extension://lfikhkjdpjcjaboanknaabncpkbgoele/";

function invokeHost(request, dataDir) {
  return new Promise((resolveResult, reject) => {
    const child = spawn(hostPath, [caller], {
      env: { ...process.env, FETCHPATH_APP_DATA_DIR: dataDir },
      stdio: ["pipe", "pipe", "pipe"],
      windowsHide: true,
    });
    const stdout = [];
    const stderr = [];
    child.stdout.on("data", (chunk) => stdout.push(chunk));
    child.stderr.on("data", (chunk) => stderr.push(chunk));
    child.on("error", reject);
    child.on("exit", (code) => {
      if (code !== 0) return reject(new Error(`native host exited ${code}: ${Buffer.concat(stderr)}`));
      const output = Buffer.concat(stdout);
      assert.ok(output.length >= 4, "native host returned a framed response");
      const length = output.readUInt32LE(0);
      assert.equal(output.length, length + 4);
      resolveResult(JSON.parse(output.subarray(4).toString("utf8")));
    });
    const body = Buffer.from(JSON.stringify(request));
    const header = Buffer.alloc(4);
    header.writeUInt32LE(body.length);
    child.stdin.end(Buffer.concat([header, body]));
  });
}

test("native host durably accepts, deduplicates, redacts, and rejects unsupported capture", { skip: process.platform !== "win32" }, async () => {
  const dataDir = await mkdtemp(join(tmpdir(), "fetchpath-browser-host-"));
  try {
    const captureId = randomUUID();
    const request = {
      schema_version: 1,
      type: "capture",
      capture_id: captureId,
      method: "GET",
      url: "https://files.example.test/archive.zip?token=top-secret",
      suggested_filename: "archive.zip",
      referrer: "https://files.example.test/downloads",
      cookies: [{
        name: "session",
        value: "cookie-secret",
        domain: "files.example.test",
        path: "/",
        secure: true,
        hostOnly: true,
        expirationDate: null,
      }],
      user_initiated: true,
    };
    const accepted = await invokeHost(request, dataDir);
    assert.equal(accepted.accepted, true);
    assert.equal(accepted.deduplicated, false);
    const duplicate = await invokeHost(request, dataDir);
    assert.equal(duplicate.accepted, true);
    assert.equal(duplicate.deduplicated, true);

    const publicRecord = await readFile(join(dataDir, "browser-inbox", `${captureId}.json`), "utf8");
    assert.doesNotMatch(publicRecord, /top-secret|cookie-secret|session/);
    const protectedSecret = await readFile(join(dataDir, "browser-secrets", `${captureId}.bin`));
    assert.doesNotMatch(protectedSecret.toString("utf8"), /top-secret|cookie-secret/);

    const rejected = await invokeHost({ ...request, capture_id: randomUUID(), method: "POST" }, dataDir);
    assert.equal(rejected.accepted, false);
    assert.equal(rejected.reason, "bridge.unsupported_method");
  } finally {
    await rm(dataDir, { recursive: true, force: true });
  }
});
