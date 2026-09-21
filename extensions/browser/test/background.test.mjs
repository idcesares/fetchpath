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
