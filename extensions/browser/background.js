if (typeof importScripts === "function" && !globalThis.FetchpathPolicy) {
  importScripts("policy.js");
}

const browserApi = globalThis.browser ?? globalThis.chrome;
const policy = globalThis.FetchpathPolicy;
const HOST_NAME = "com.fetchpath.browser";
const MENU_SEND_LINK = "fetchpath-send-link";
const MENU_EXCLUDE_SITE = "fetchpath-exclude-site";
const MENU_INCLUDE_SITE = "fetchpath-include-site";

async function readState() {
  const state = await browserApi.storage.local.get(["excludedOrigins", "events"]);
  return {
    excludedOrigins: Array.isArray(state.excludedOrigins) ? state.excludedOrigins : [],
    events: Array.isArray(state.events) ? state.events : [],
  };
}

async function recordEvent(event) {
  const state = await readState();
  state.events.push({ ...event, observedAt: Date.now() });
  await browserApi.storage.local.set({ events: state.events.slice(-100) });
}

async function setOriginExcluded(rawUrl, excluded) {
  const origin = policy.normalizeOrigin(rawUrl);
  if (!origin) return false;
  const state = await readState();
  const next = new Set(state.excludedOrigins);
  if (excluded) next.add(origin);
  else next.delete(origin);
  await browserApi.storage.local.set({ excludedOrigins: [...next].sort() });
  await setBadge(excluded ? "—" : "✓", excluded ? "#6b7280" : "#19745f");
  return true;
}

async function ensureOriginPermission(url) {
  const pattern = `${url.protocol}//${url.hostname}/*`;
  if (await browserApi.permissions.contains({ origins: [pattern] })) return true;
  return browserApi.permissions.request({ origins: [pattern] });
}

async function setBadge(text, color) {
  if (!browserApi.action) return;
  await browserApi.action.setBadgeBackgroundColor({ color });
  await browserApi.action.setBadgeText({ text });
  setTimeout(() => void browserApi.action.setBadgeText({ text: "" }), 2500);
}

async function sendLink(rawUrl, tab) {
  const url = policy.parseHttpUrl(rawUrl);
  const state = await readState();
  if (!url) {
    await recordEvent({ accepted: false, reason: "unsupported_scheme", redactedUrl: "<unsupported-url>" });
    await setBadge("!", "#a33a2b");
    return { accepted: false, fallback: true, reason: "unsupported_scheme" };
  }
  if (policy.isExcluded(rawUrl, state.excludedOrigins)) {
    await recordEvent({ accepted: false, reason: "site_excluded", redactedUrl: policy.redactUrl(rawUrl) });
    await setBadge("—", "#6b7280");
    return { accepted: false, fallback: true, reason: "site_excluded" };
  }
  const permissionGranted = await ensureOriginPermission(url);
  if (!permissionGranted) {
    await recordEvent({ accepted: false, reason: "site_permission_denied", redactedUrl: policy.redactUrl(rawUrl) });
    await setBadge("!", "#a33a2b");
    return { accepted: false, fallback: true, reason: "site_permission_denied" };
  }

  const cookies = await browserApi.cookies.getAll({ url: rawUrl });
  const request = {
    schema_version: 1,
    type: "capture",
    capture_id: crypto.randomUUID(),
    method: "GET",
    url: rawUrl,
    suggested_filename: policy.suggestedFilename(rawUrl),
    referrer: policy.normalizeOrigin(tab?.url) === url.origin ? tab.url : null,
    cookies: cookies.map((cookie) => ({
      name: cookie.name,
      value: cookie.value,
      domain: cookie.domain,
      path: cookie.path,
      secure: cookie.secure,
      hostOnly: cookie.hostOnly,
      expirationDate: cookie.expirationDate ?? null,
    })),
    user_initiated: true,
  };

  let response;
  try {
    response = await browserApi.runtime.sendNativeMessage(HOST_NAME, request);
  } catch (error) {
    response = { accepted: false, reason: "native_host_unavailable" };
    await recordEvent({
      captureId: request.capture_id,
      accepted: false,
      fallback: true,
      reason: response.reason,
      redactedUrl: policy.redactUrl(rawUrl),
    });
    await setBadge("!", "#a33a2b");
    return response;
  }

  await recordEvent({
    captureId: request.capture_id,
    accepted: response?.accepted === true,
    deduplicated: response?.deduplicated === true,
    fallback: response?.accepted !== true,
    reason: response?.reason ?? null,
    redactedUrl: policy.redactUrl(rawUrl),
    cookieCount: cookies.length,
  });
  await setBadge(response?.accepted === true ? "✓" : "!", response?.accepted === true ? "#19745f" : "#a33a2b");
  return response;
}

async function createMenus() {
  await browserApi.contextMenus.removeAll();
  browserApi.contextMenus.create({ id: MENU_SEND_LINK, title: "Send link to Fetchpath", contexts: ["link"] });
  browserApi.contextMenus.create({ id: MENU_EXCLUDE_SITE, title: "Exclude this site from Fetchpath", contexts: ["page"] });
  browserApi.contextMenus.create({ id: MENU_INCLUDE_SITE, title: "Allow this site in Fetchpath", contexts: ["page"] });
}

browserApi.runtime.onInstalled.addListener(() => void createMenus());
browserApi.runtime.onStartup.addListener(() => void createMenus());
void createMenus();

browserApi.contextMenus.onClicked.addListener((info, tab) => {
  if (info.menuItemId === MENU_SEND_LINK && info.linkUrl) {
    void sendLink(info.linkUrl, tab);
  } else if (info.menuItemId === MENU_EXCLUDE_SITE && tab?.url) {
    void setOriginExcluded(tab.url, true);
  } else if (info.menuItemId === MENU_INCLUDE_SITE && tab?.url) {
    void setOriginExcluded(tab.url, false);
  }
});

browserApi.runtime.onMessage.addListener((message, sender) => {
  if (message?.type === "get_state") return readState();
  if (message?.type === "send_link" && message.url) return sendLink(message.url, sender.tab);
  if (message?.type === "set_excluded" && message.url) return setOriginExcluded(message.url, Boolean(message.excluded));
  return undefined;
});
