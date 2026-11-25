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
        async remove(key) { delete state[key]; },
      },
    },
    permissions: {
      async contains() { return true; },
      async request() { return true; },
      onAdded: noOpEvent,
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
    clearTimeout() {},
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

test("the popup's send names the page from its title", async () => {
  const app = harness();
  const sent = await app.sendMessage(
    { type: "send_link", url: "https://example.test/", pageTitle: "Example Domain" },
    {},
  );
  assert.equal(sent.accepted, true);
  assert.equal(app.nativeRequests[0].request.suggested_filename, "Example Domain.html");
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
    storage: { local: { async get() { return { ...state }; }, async set(values) { Object.assign(state, values); }, async remove(key) { delete state[key]; } } },
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
    clearTimeout() {},
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
  // One idempotent retry on MV3 wake hiccup reuses the same capture_id.
  assert.equal(app.nativeRequests.length, 2);
  assert.equal(app.nativeRequests[0].capture_id, app.nativeRequests[1].capture_id);
  // The handed-back download is not captured again.
  await app.download({ ...started, id: 8 });
  assert.equal(app.nativeRequests.length, 2);
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

test("the page menu names an extensionless page from its title; link and media sends keep the URL's name", async () => {
  const app = automaticHarness();
  const tab = { url: "https://www.linkedin.com/in/alcinei-oliveira-42781750/", title: "Alcinei Oliveira" };
  await app.menu({ menuItemId: "fetchpath-send-page" }, tab);
  assert.equal(app.nativeRequests.at(-1).suggested_filename, "Alcinei Oliveira.html");
  await app.menu({ menuItemId: "fetchpath-send-link", linkUrl: "https://www.linkedin.com/in/someone-else" }, tab);
  assert.equal(app.nativeRequests.at(-1).suggested_filename, "someone-else");
  await app.menu({ menuItemId: "fetchpath-send-media" }, tab);
  assert.equal(app.nativeRequests.at(-1).suggested_filename, "alcinei-oliveira-42781750");
});

test("automatic capture leaves POST downloads on the browser path", async () => {
  const app = automaticHarness();
  await app.enable();
  await app.download({ ...started, id: 12, method: "POST" });
  assert.equal(app.nativeRequests.length, 0);
  assert.deepEqual(app.calls, []);
});

test("automatic capture preserves a safe browser-supplied filename", async () => {
  const app = automaticHarness();
  await app.enable();
  await app.download({
    ...started,
    id: 14,
    url: "https://files.example.test/download?id=99",
    filename: "C:\\Users\\alice\\Downloads\\installer-v2.exe",
  });
  assert.equal(app.nativeRequests.length, 1);
  assert.equal(app.nativeRequests[0].suggested_filename, "installer-v2.exe");
});

test("context menu sends immediately without prompting for site permission and retries one wake hiccup", async () => {
  const state = {};
  const nativeRequests = [];
  let permissionRequestCalls = 0;
  let cookiesReadCalls = 0;
  let menuListener;
  let onInstalledListener;
  let onAddedListener;
  let removeAllCalls = 0;
  let failOnce = true;
  const chrome = {
    storage: {
      local: {
        async get(keys) {
          if (typeof keys === "string") return { [keys]: state[keys] };
          return { ...state };
        },
        async set(values) {
          Object.assign(state, values);
        },
        async remove(key) {
          delete state[key];
        },
      },
    },
    permissions: {
      async contains({ permissions }) {
        // Automatic capture permission check returns true; per-site origin permission is NOT granted.
        return Array.isArray(permissions) && permissions.includes("downloads");
      },
      request() {
        permissionRequestCalls += 1;
        throw new Error("manual sends must not prompt for site permission");
      },
      onAdded: { addListener(fn) { onAddedListener = fn; } },
    },
    cookies: {
      async getAll() {
        cookiesReadCalls += 1;
        return [{ name: "secret", value: "s", domain: "cdn.example.test", path: "/", secure: true, hostOnly: true }];
      },
    },
    runtime: {
      onInstalled: { addListener(fn) { onInstalledListener = fn; } },
      onStartup: { addListener() {} },
      onMessage: { addListener() {} },
      async sendNativeMessage(host, request) {
        nativeRequests.push(request);
        if (failOnce) {
          failOnce = false;
          throw new Error("Native host has exited.");
        }
        return { accepted: true, deduplicated: false };
      },
    },
    contextMenus: {
      async removeAll() { removeAllCalls += 1; },
      create() {},
      onClicked: { addListener(fn) { menuListener = fn; } },
    },
    action: {
      async setBadgeBackgroundColor() {},
      async setBadgeText() {},
    },
  };
  const context = vm.createContext({
    URL,
    chrome,
    crypto: { randomUUID: () => "00000000-0000-4000-8000-000000000003" },
    setTimeout(fn, ms) {
      if (ms < 1000) fn();
    },
    clearTimeout() {},
  });
  vm.runInContext(readFileSync(new URL("../policy.js", import.meta.url), "utf8"), context);
  vm.runInContext(readFileSync(new URL("../background.js", import.meta.url), "utf8"), context);
  const settle = () => new Promise((resolve) => setImmediate(resolve));
  for (let i = 0; i < 10; i++) await settle();

  // Waking the service worker must not wipe existing context menus until onInstalled/onStartup.
  assert.equal(removeAllCalls, 0);
  onInstalledListener();
  for (let i = 0; i < 10; i++) await settle();
  assert.equal(removeAllCalls, 1);

  menuListener({ menuItemId: "fetchpath-send-link", linkUrl: "https://cdn.example.test/release.zip" }, { url: "https://example.test/" });
  for (let i = 0; i < 20; i++) await settle();

  assert.equal(permissionRequestCalls, 0, "manual sends never prompt for per-site permission");
  assert.equal(cookiesReadCalls, 0, "cookies are never read when the site is not granted");
  assert.equal(nativeRequests.length, 2, "one idempotent retry recovers from a cold-wake native host error");
  assert.equal(nativeRequests[0].capture_id, nativeRequests[1].capture_id);
  assert.deepEqual(Array.from(nativeRequests[0].cookies), []);
  assert.equal(state.events.at(-1)?.accepted, true);

  // Pending auto-capture completes when the permission grant arrives after popup dismissal,
  // while non-finite timestamps fail closed.
  state.pendingAutoCapture = { at: NaN };
  await onAddedListener({ permissions: ["downloads"], origins: ["http://*/*", "https://*/*"] });
  assert.equal(state.autoCapture, undefined);
  assert.equal(state.pendingAutoCapture, undefined);

  state.pendingAutoCapture = { at: Date.now() };
  await onAddedListener({ permissions: ["downloads"], origins: ["http://*/*", "https://*/*"] });
  assert.equal(state.autoCapture, true);
  assert.equal(state.pendingAutoCapture, undefined);
});

test("popup send_link works without site permission, startup sweeps legacy pendingSend, and disabling auto-capture revokes broad permissions", async () => {
  const localState = {
    pendingSend: { url: "https://private.example.test/file?token=secret", pageTitle: "Secret", at: Date.now() },
  };
  const sessionState = {
    pendingSend: { url: "https://private.example.test/file?token=secret2", pageTitle: "Secret2", at: Date.now() },
  };
  const nativeRequests = [];
  const removedPermissions = [];
  let cookiesReadCalls = 0;
  let messageListener;
  const chrome = {
    storage: {
      local: {
        async get(keys) {
          if (typeof keys === "string") return { [keys]: localState[keys] };
          return { ...localState };
        },
        async set(values) { Object.assign(localState, values); },
        async remove(key) { delete localState[key]; },
      },
      session: {
        async get(keys) {
          if (typeof keys === "string") return { [keys]: sessionState[keys] };
          return { ...sessionState };
        },
        async set(values) { Object.assign(sessionState, values); },
        async remove(key) { delete sessionState[key]; },
      },
    },
    permissions: {
      async contains() { return false; },
      request() { throw new Error("popup send_link must not prompt"); },
      async remove(perms) { removedPermissions.push(perms); return true; },
      onAdded: { addListener() {} },
    },
    cookies: {
      async getAll() {
        cookiesReadCalls += 1;
        return [];
      },
    },
    runtime: {
      onInstalled: { addListener() {} },
      onStartup: { addListener() {} },
      onMessage: { addListener(fn) { messageListener = fn; } },
      async sendNativeMessage(_host, request) {
        nativeRequests.push(request);
        return { accepted: true, deduplicated: false };
      },
    },
    contextMenus: { async removeAll() {}, create() {}, onClicked: { addListener() {} } },
    action: { async setBadgeBackgroundColor() {}, async setBadgeText() {} },
  };
  const context = vm.createContext({
    URL,
    chrome,
    crypto: { randomUUID: () => "00000000-0000-4000-8000-000000000005" },
    setTimeout(fn, ms) { if (ms < 1000) fn(); },
    clearTimeout() {},
  });
  vm.runInContext(readFileSync(new URL("../policy.js", import.meta.url), "utf8"), context);
  vm.runInContext(readFileSync(new URL("../background.js", import.meta.url), "utf8"), context);
  const settle = () => new Promise((resolve) => setImmediate(resolve));
  for (let i = 0; i < 10; i++) await settle();

  // Startup sweeps any legacy unredacted pendingSend from both session and local storage.
  assert.equal(localState.pendingSend, undefined);
  assert.equal(sessionState.pendingSend, undefined);

  const sendMessage = (msg) => new Promise((resolve) => messageListener(msg, {}, resolve));
  const sent = await sendMessage({
    type: "send_link",
    url: "https://public.example.test/video",
    pageTitle: "Public Video",
  });
  assert.equal(sent.accepted, true);
  assert.equal(cookiesReadCalls, 0);
  assert.equal(nativeRequests.length, 1);
  assert.deepEqual(Array.from(nativeRequests[0].cookies), []);

  // Turning off automatic capture revokes broad wildcard permissions.
  await sendMessage({ type: "set_auto_capture", enabled: false, claimPending: true });
  assert.equal(localState.autoCapture, false);
  assert.equal(removedPermissions.length, 1);
  assert.deepEqual(Array.from(removedPermissions[0].permissions), ["downloads"]);
  assert.deepEqual(Array.from(removedPermissions[0].origins), ["http://*/*", "https://*/*"]);
});
