if (typeof importScripts === "function" && !globalThis.FetchpathPolicy) {
  importScripts("policy.js");
}

const browserApi = globalThis.browser ?? globalThis.chrome;
const policy = globalThis.FetchpathPolicy;
const HOST_NAME = "com.fetchpath.browser";
const SCHEMA_VERSION = 1;
const MENU_SEND_LINK = "fetchpath-send-link";
const MENU_SEND_PAGE = "fetchpath-send-page";
const MENU_SEND_MEDIA = "fetchpath-send-media";
const MENU_EXCLUDE_SITE = "fetchpath-exclude-site";
const MENU_INCLUDE_SITE = "fetchpath-include-site";

async function readState() {
  const state = await browserApi.storage.local.get(["excludedOrigins", "events", "autoCapture"]);
  return {
    excludedOrigins: Array.isArray(state.excludedOrigins) ? state.excludedOrigins : [],
    events: Array.isArray(state.events) ? state.events : [],
    autoCapture: state.autoCapture === true,
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

function originPattern(url) {
  return `${url.protocol}//${url.hostname}/*`;
}

/**
 * Cookies are read only for sites the person has allowed. A request needs a
 * user gesture, so `prompt` is false on the automatic path, where the site must
 * already be allowed.
 */
async function ensureOriginPermission(url, prompt = true) {
  const pattern = originPattern(url);
  if (await browserApi.permissions.contains({ origins: [pattern] })) return true;
  if (!prompt) return false;
  return browserApi.permissions.request({ origins: [pattern] });
}

async function setBadge(text, color) {
  if (!browserApi.action) return;
  await browserApi.action.setBadgeBackgroundColor({ color });
  await browserApi.action.setBadgeText({ text });
  setTimeout(() => void browserApi.action.setBadgeText({ text: "" }), 2500);
}

/** Asks the Fetchpath host whether it is installed and answering. */
async function probe() {
  try {
    const response = await browserApi.runtime.sendNativeMessage(HOST_NAME, {
      type: "probe",
      schema_version: SCHEMA_VERSION,
    });
    return { connected: response?.accepted === true, reason: response?.reason ?? null };
  } catch (error) {
    return { connected: false, reason: "native_host_unavailable" };
  }
}

async function sendLink(rawUrl, tab, { prompt = true } = {}) {
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
  const permissionGranted = await ensureOriginPermission(url, prompt);
  if (!permissionGranted) {
    await recordEvent({ accepted: false, reason: "site_permission_denied", redactedUrl: policy.redactUrl(rawUrl) });
    await setBadge("!", "#a33a2b");
    return { accepted: false, fallback: true, reason: "site_permission_denied" };
  }

  const cookies = await browserApi.cookies.getAll({ url: rawUrl });
  const request = {
    schema_version: SCHEMA_VERSION,
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
    // Every path here starts with the person: a menu choice, the popup, or a
    // download they clicked after turning on automatic capture.
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

/* Automatic capture --------------------------------------------------------
   Off until the person turns it on in the popup, which also grants the
   downloads permission and access to all sites. When on, a download the
   browser starts is cancelled and handed to Fetchpath; if Fetchpath cannot take
   it, the browser download is started again, so nothing is ever lost. */

const handingBack = new Set();

async function onDownloadCreated(item) {
  const state = await readState();
  if (!state.autoCapture) return;
  const rawUrl = item.finalUrl || item.url;
  const url = policy.parseHttpUrl(rawUrl);
  if (!url || item.state !== "in_progress") return;
  if (handingBack.has(rawUrl)) {
    handingBack.delete(rawUrl);
    return;
  }
  if (policy.isExcluded(rawUrl, state.excludedOrigins)) return;
  // Without cookie access Fetchpath could fail where the browser would not.
  if (!(await ensureOriginPermission(url, false))) return;

  await browserApi.downloads.cancel(item.id);
  await browserApi.downloads.erase({ id: item.id });
  const response = await sendLink(rawUrl, { url: item.referrer || null }, { prompt: false });
  if (response?.accepted !== true) {
    handingBack.add(rawUrl);
    await browserApi.downloads.download({ url: rawUrl });
  }
}

let listeningForDownloads = false;
async function watchDownloads() {
  if (listeningForDownloads || !browserApi.downloads?.onCreated) return;
  listeningForDownloads = true;
  browserApi.downloads.onCreated.addListener((item) => void onDownloadCreated(item));
}

async function createMenus() {
  await browserApi.contextMenus.removeAll();
  browserApi.contextMenus.create({ id: MENU_SEND_LINK, title: "Send link to Fetchpath", contexts: ["link"] });
  browserApi.contextMenus.create({ id: MENU_SEND_MEDIA, title: "Send this video to Fetchpath", contexts: ["video", "audio"] });
  browserApi.contextMenus.create({ id: MENU_SEND_PAGE, title: "Send this page to Fetchpath", contexts: ["page"] });
  browserApi.contextMenus.create({ id: MENU_EXCLUDE_SITE, title: "Exclude this site from Fetchpath", contexts: ["page"] });
  browserApi.contextMenus.create({ id: MENU_INCLUDE_SITE, title: "Allow this site in Fetchpath", contexts: ["page"] });
}

browserApi.runtime.onInstalled.addListener(() => void createMenus());
browserApi.runtime.onStartup.addListener(() => void createMenus());
void createMenus();
void watchDownloads();
browserApi.permissions.onAdded?.addListener(() => void watchDownloads());

browserApi.contextMenus.onClicked.addListener((info, tab) => {
  if (info.menuItemId === MENU_SEND_LINK && info.linkUrl) {
    void sendLink(info.linkUrl, tab);
  } else if (info.menuItemId === MENU_SEND_MEDIA) {
    // A video element's source is often a blob: stream the page builds; the
    // page itself is what Fetchpath's media engine understands.
    const source = policy.parseHttpUrl(info.srcUrl ?? "") ? info.srcUrl : tab?.url;
    if (source) void sendLink(source, tab);
  } else if (info.menuItemId === MENU_SEND_PAGE && tab?.url) {
    void sendLink(tab.url, tab);
  } else if (info.menuItemId === MENU_EXCLUDE_SITE && tab?.url) {
    void setOriginExcluded(tab.url, true);
  } else if (info.menuItemId === MENU_INCLUDE_SITE && tab?.url) {
    void setOriginExcluded(tab.url, false);
  }
});

browserApi.runtime.onMessage.addListener((message, sender) => {
  if (message?.type === "get_state") return readState();
  if (message?.type === "probe") return probe();
  if (message?.type === "send_link" && message.url) {
    // From the popup the site permission was already requested there, inside
    // the click, because a request made here would have lost the gesture.
    return sendLink(message.url, sender.tab ?? { url: message.url }, { prompt: false });
  }
  if (message?.type === "set_auto_capture") {
    return browserApi.storage.local
      .set({ autoCapture: Boolean(message.enabled) })
      .then(() => watchDownloads())
      .then(() => ({ autoCapture: Boolean(message.enabled) }));
  }
  if (message?.type === "set_excluded" && message.url) return setOriginExcluded(message.url, Boolean(message.excluded));
  return undefined;
});
