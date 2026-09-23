import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";

function harness() {
  const state = {};
  const nativeRequests = [];
  let messageListener;
  const noOpEvent = { addListener() {} };
  const browser = {
    storage: {
      local: {
        async get() { return { ...state }; },
        async set(values) { Object.assign(state, values); },
      },
    },
    permissions: {
      async contains() { return true; },
      async request() { return true; },
    },
    cookies: {
      async getAll() {
        return [{
          name: "session",
          value: "cookie-secret",
          domain: "files.example.test",
          path: "/",
          secure: true,
          hostOnly: true,
        }];
      },
    },
    runtime: {
      onInstalled: noOpEvent,
      onStartup: noOpEvent,
      onMessage: { addListener(listener) { messageListener = listener; } },
      async sendNativeMessage(host, request) {
        nativeRequests.push({ host, request });
        return { accepted: true, deduplicated: false };
      },
    },
    contextMenus: {
      async removeAll() {},
      create() {},
      onClicked: noOpEvent,
    },
    action: {
      async setBadgeBackgroundColor() {},
      async setBadgeText() {},
    },
  };
  const context = vm.createContext({
    URL,
    browser,
    crypto: { randomUUID: () => "00000000-0000-4000-8000-000000000001" },
    setTimeout() {},
  });
  vm.runInContext(readFileSync(new URL("../policy.js", import.meta.url), "utf8"), context);
  vm.runInContext(readFileSync(new URL("../background.js", import.meta.url), "utf8"), context);
  return { state, nativeRequests, sendMessage: (message, sender) => messageListener(message, sender) };
}

test("excluded origins never reach native messaging and explicit authenticated sends stay redacted", async () => {
  const app = harness();
  const page = "https://files.example.test/downloads?account=private";
  const link = "https://files.example.test/archive.zip?token=top-secret";
  await app.sendMessage({ type: "set_excluded", url: page, excluded: true }, {});
  const excluded = await app.sendMessage({ type: "send_link", url: link }, { tab: { url: page } });
  assert.equal(excluded.reason, "site_excluded");
  assert.equal(app.nativeRequests.length, 0);

  await app.sendMessage({ type: "set_excluded", url: page, excluded: false }, {});
  const accepted = await app.sendMessage({ type: "send_link", url: link }, { tab: { url: page } });
  assert.equal(accepted.accepted, true);
  assert.equal(app.nativeRequests.length, 1);
  assert.equal(app.nativeRequests[0].request.cookies[0].value, "cookie-secret");
  assert.equal(app.nativeRequests[0].request.user_initiated, true);
  const persistedExtensionState = JSON.stringify(app.state);
  assert.doesNotMatch(persistedExtensionState, /top-secret|cookie-secret|account=private/);
});

function automaticHarness({ hostAccepts = true, siteAllowed = true } = {}) {
  const state = {};
  const nativeRequests = [];
  const calls = [];
  let downloadListener;
  let menuListener;
  let messageListener;
  const noOpEvent = { addListener() {} };
  const browser = {
    storage: { local: { async get() { return { ...state }; }, async set(values) { Object.assign(state, values); } } },
    permissions: {
      async contains({ origins }) { return siteAllowed || !origins; },
      async request() { throw new Error("no user gesture on the automatic path"); },
      onAdded: noOpEvent,
    },
    cookies: { async getAll() { return []; } },
    runtime: {
      onInstalled: noOpEvent,
      onStartup: noOpEvent,
      onMessage: { addListener(listener) { messageListener = listener; } },
      async sendNativeMessage(host, request) {
        nativeRequests.push(request);
        if (!hostAccepts) throw new Error("host missing");
        return { accepted: true };
      },
    },
    contextMenus: { async removeAll() {}, create() {}, onClicked: { addListener(listener) { menuListener = listener; } } },
    action: { async setBadgeBackgroundColor() {}, async setBadgeText() {} },
    downloads: {
      onCreated: { addListener(listener) { downloadListener = listener; } },
      async cancel(id) { calls.push(["cancel", id]); },
      async erase(query) { calls.push(["erase", query.id]); },
      async download(options) { calls.push(["download", options.url]); },
    },
  };
  const context = vm.createContext({
    URL,
    browser,
    crypto: { randomUUID: () => "00000000-0000-4000-8000-000000000002" },
    setTimeout() {},
  });
  vm.runInContext(readFileSync(new URL("../policy.js", import.meta.url), "utf8"), context);
  vm.runInContext(readFileSync(new URL("../background.js", import.meta.url), "utf8"), context);
  const settle = () => new Promise((resolve) => setImmediate(resolve));
  return {
    nativeRequests,
    calls,
    enable: () => messageListener({ type: "set_auto_capture", enabled: true }, {}),
    download: async (item) => { downloadListener(item); for (let i = 0; i < 20; i++) await settle(); },
    menu: async (info, tab) => { menuListener(info, tab); for (let i = 0; i < 20; i++) await settle(); },
  };
}

const started = { id: 7, url: "https://files.example.test/big.iso", state: "in_progress", referrer: "https://files.example.test/" };

test("automatic capture does nothing until the person turns it on", async () => {
  const app = automaticHarness();
  await app.download(started);
  assert.equal(app.nativeRequests.length, 0);
  assert.deepEqual(app.calls, []);
});

test("an automatic capture moves the download from the browser to Fetchpath", async () => {
  const app = automaticHarness();
  await app.enable();
  await app.download(started);
  assert.equal(app.nativeRequests.length, 1);
  assert.equal(app.nativeRequests[0].url, started.url);
  assert.deepEqual(app.calls, [["cancel", 7], ["erase", 7]]);
});

test("if Fetchpath cannot take a download the browser gets it back", async () => {
  const app = automaticHarness({ hostAccepts: false });
  await app.enable();
  await app.download(started);
  assert.deepEqual(app.calls, [["cancel", 7], ["erase", 7], ["download", started.url]]);
  // The handed-back download is not captured again.
  await app.download({ ...started, id: 8 });
  assert.equal(app.nativeRequests.length, 1);
});

test("a site without permission is left to the browser, with no prompt", async () => {
  const app = automaticHarness({ siteAllowed: false });
  await app.enable();
  await app.download(started);
  assert.equal(app.nativeRequests.length, 0);
  assert.deepEqual(app.calls, []);
});

test("sending a video element falls back to its page when the source is a stream", async () => {
  const app = automaticHarness();
  const page = { url: "https://video.example.test/watch?v=abc" };
  await app.menu({ menuItemId: "fetchpath-send-media", srcUrl: "blob:https://video.example.test/1" }, page);
  assert.equal(app.nativeRequests[0].url, page.url);
});
