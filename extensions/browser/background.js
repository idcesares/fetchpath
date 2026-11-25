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
const PENDING_TTL_MS = 120000;
const AUTO_CAPTURE_PERMISSIONS = {
  permissions: ["downloads"],
  origins: ["http://*/*", "https://*/*"],
};

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

async function setAutoCapture(enabled) {
  const value = Boolean(enabled);
  await browserApi.storage.local.set({ autoCapture: value });
  if (value) {
    await watchDownloads();
  } else if (typeof browserApi.permissions?.remove === "function") {
    // Revoke the broad wildcard host access granted for automatic capture so
    // per-site cookie sharing remains explicit and revocable.
    await browserApi.permissions.remove(AUTO_CAPTURE_PERMISSIONS).catch(() => false);
  }
  return { autoCapture: value };
}

function originPattern(url) {
  return `${url.protocol}//${url.hostname}/*`;
}

/**
 * Checks whether the person has already granted cookie access for this site.
 * Never prompts: manual sends work immediately without cookies, and automatic
 * capture leaves ungranted sites to the browser.
 */
async function hasOriginPermission(url) {
  return Boolean(await browserApi.permissions.contains({ origins: [originPattern(url)] }).catch(() => false));
}

async function setBadge(text, color) {
  if (!browserApi.action) return;
  await browserApi.action.setBadgeBackgroundColor({ color });
  await browserApi.action.setBadgeText({ text });
  setTimeout(() => void browserApi.action.setBadgeText({ text: "" }), 2500);
}

// The host answers within about 3 s even while it starts the engine. A host
// that never answers must not leave a cancelled browser download unrecovered.
const HOST_TIMEOUT_MS = 10000;

function sendToHost(message) {
  let timer;
  const timeout = new Promise((_, reject) => {
    timer = setTimeout(() => reject(new Error("native host timed out")), HOST_TIMEOUT_MS);
  });
  return Promise.race([browserApi.runtime.sendNativeMessage(HOST_NAME, message), timeout]).finally(() =>
    clearTimeout(timer),
  );
}

/** Asks the Fetchpath host whether it is installed and answering. */
async function probe() {
  // The first native message after the worker wakes can fail once; a second try
  // tells a real absence from a hiccup.
  for (let attempt = 0; ; attempt += 1) {
    try {
      const response = await sendToHost({ type: "probe", schema_version: SCHEMA_VERSION });
      return { connected: response?.accepted === true, reason: response?.reason ?? null };
    } catch (error) {
      if (attempt >= 1) return { connected: false, reason: "native_host_unavailable" };
    }
  }
}

async function sendLink(
  rawUrl,
  tab,
  { pageTitle, suggestedFilename, requireSitePermission = false, captureId } = {},
) {
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
  const hasSitePermission = await hasOriginPermission(url);
  if (requireSitePermission && !hasSitePermission) {
    await recordEvent({ accepted: false, reason: "site_permission_denied", redactedUrl: policy.redactUrl(rawUrl) });
    await setBadge("!", "#a33a2b");
    return { accepted: false, fallback: true, reason: "site_permission_denied" };
  }

  let cookies = [];
  if (hasSitePermission) {
    const cookieUrl = new URL(url.href);
    cookieUrl.hash = "";
    const cookieQuery = { url: cookieUrl.href };
    if (typeof tab?.cookieStoreId === "string" && tab.cookieStoreId) {
      cookieQuery.storeId = tab.cookieStoreId;
    }
    const rawCookies = await browserApi.cookies.getAll(cookieQuery).catch(() => []);
    cookies = policy.filterCookiesForUrl(rawCookies, url);
  }
  const filename =
    typeof suggestedFilename === "string" && suggestedFilename.trim()
      ? policy.sanitizeFilename(suggestedFilename, policy.suggestedFilename(rawUrl))
      : pageTitle === undefined
        ? policy.suggestedFilename(rawUrl)
        : policy.pageFilename(rawUrl, pageTitle);
  const request = {
    schema_version: SCHEMA_VERSION,
    type: "capture",
    capture_id: typeof captureId === "string" && captureId ? captureId : crypto.randomUUID(),
    method: "GET",
    url: rawUrl,
    suggested_filename: filename,
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

  // Like probe(), the first native message after an MV3 service-worker wake can
  // fail once. Reusing the exact same capture_id makes one retry idempotent.
  let response;
  for (let attempt = 0; ; attempt += 1) {
    try {
      response = await sendToHost(request);
      break;
    } catch (error) {
      // A timeout is not "unavailable": the host may still have taken the
      // capture, so the hand-back can download the file twice rather than lose it.
      const timedOut = error?.message === "native host timed out";
      if (!timedOut && attempt < 1) continue;
      response = { accepted: false, reason: timedOut ? "native_host_timeout" : "native_host_unavailable" };
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

const HAND_BACK_TTL_MS = 30000;
const handingBackMemory = new Map();
let handingBackQueue = Promise.resolve();

function withHandingBackLock(fn) {
  const next = handingBackQueue.then(fn, fn);
  handingBackQueue = next.then(() => undefined, () => undefined);
  return next;
}

async function readHandingBack() {
  const now = Date.now();
  let entries = [];
  if (browserApi.storage?.session?.get) {
    try {
      const stored = await browserApi.storage.session.get("handingBack");
      if (Array.isArray(stored?.handingBack)) entries = stored.handingBack;
    } catch {
      // Fall back to in-memory map if session storage is unavailable.
    }
  }
  const map = new Map(handingBackMemory);
  for (const [k, at] of entries) {
    if (typeof k === "string" && typeof at === "number") map.set(k, at);
  }
  for (const [k, at] of map) {
    if (now - at > HAND_BACK_TTL_MS) map.delete(k);
  }
  return map;
}

async function writeHandingBack(map) {
  handingBackMemory.clear();
  for (const [k, at] of map) handingBackMemory.set(k, at);
  if (browserApi.storage?.session?.set) {
    try {
      await browserApi.storage.session.set({ handingBack: [...map.entries()] });
    } catch {
      // In-memory map remains authoritative for this worker lifetime.
    }
  }
}

function consumeHandingBack(urls) {
  return withHandingBackLock(async () => {
    const map = await readHandingBack();
    let matched = false;
    for (const candidate of urls) {
      if (candidate && map.delete(candidate)) matched = true;
    }
    if (matched) await writeHandingBack(map);
    return matched;
  });
}

function markHandingBack(urls, add) {
  return withHandingBackLock(async () => {
    const map = await readHandingBack();
    const now = Date.now();
    for (const candidate of urls) {
      if (!candidate) continue;
      if (add) map.set(candidate, now);
      else map.delete(candidate);
    }
    await writeHandingBack(map);
  });
}

async function onDownloadCreated(item) {
  const state = await readState();
  if (!state.autoCapture) return;
  if (item.method && String(item.method).toUpperCase() !== "GET") return;
  const rawUrl = item.finalUrl || item.url;
  const url = policy.parseHttpUrl(rawUrl);
  if (!url || item.state !== "in_progress") return;
  const candidates = [item.url, item.finalUrl].filter(Boolean);
  if (await consumeHandingBack(candidates)) return;
  if (policy.isExcluded(rawUrl, state.excludedOrigins)) return;
  // Without cookie access Fetchpath could fail where the browser would not.
  if (!(await hasOriginPermission(url))) return;

  try {
    await browserApi.downloads.cancel(item.id);
  } catch (error) {
    return; // Not cancelled: the browser keeps the download.
  }
  await browserApi.downloads.erase({ id: item.id }).catch(() => {});
  let response;
  const leafName =
    typeof item.filename === "string" && item.filename.trim()
      ? item.filename.split(/[/\\]/).filter(Boolean).at(-1)
      : undefined;
  try {
    response = await sendLink(
      rawUrl,
      { url: item.referrer || null },
      { requireSitePermission: true, suggestedFilename: leafName },
    );
  } catch (error) {
    // Any failure after the cancel returns the download to the browser.
    response = { accepted: false, reason: "capture_failed" };
  }
  if (response?.accepted !== true) {
    await markHandingBack(candidates, true);
    try {
      await browserApi.downloads.download({ url: rawUrl });
    } catch {
      await markHandingBack(candidates, false);
    }
  }
}

let listeningForDownloads = false;
async function watchDownloads() {
  if (listeningForDownloads || !browserApi.downloads?.onCreated) return;
  listeningForDownloads = true;
  browserApi.downloads.onCreated.addListener((item) => void onDownloadCreated(item));
}

let menuSetupQueue = Promise.resolve();
function createMenus() {
  const run = async () => {
    await browserApi.contextMenus.removeAll();
    const ignoreLastError = () => void browserApi.runtime?.lastError;
    browserApi.contextMenus.create(
      { id: MENU_SEND_LINK, title: "Send link to Fetchpath", contexts: ["link"] },
      ignoreLastError,
    );
    browserApi.contextMenus.create(
      { id: MENU_SEND_MEDIA, title: "Send this video to Fetchpath", contexts: ["video", "audio"] },
      ignoreLastError,
    );
    browserApi.contextMenus.create(
      { id: MENU_SEND_PAGE, title: "Send this page to Fetchpath", contexts: ["page"] },
      ignoreLastError,
    );
    browserApi.contextMenus.create(
      { id: MENU_EXCLUDE_SITE, title: "Exclude this site from Fetchpath", contexts: ["page"] },
      ignoreLastError,
    );
    browserApi.contextMenus.create(
      { id: MENU_INCLUDE_SITE, title: "Allow this site in Fetchpath", contexts: ["page"] },
      ignoreLastError,
    );
  };
  menuSetupQueue = menuSetupQueue.then(run, run);
  return menuSetupQueue;
}

let pendingActionQueue = Promise.resolve();

function withPendingLock(fn) {
  const next = pendingActionQueue.then(fn, fn);
  pendingActionQueue = next.then(() => undefined, () => undefined);
  return next;
}

function pendingStorage() {
  return browserApi.storage?.session ?? browserApi.storage.local;
}

function isFreshPending(entry) {
  if (!entry || typeof entry.at !== "number" || !Number.isFinite(entry.at)) return false;
  const age = Date.now() - entry.at;
  return age >= 0 && age <= PENDING_TTL_MS;
}

async function readPendingKey(key) {
  const primary = pendingStorage();
  const fromPrimary = await primary.get(key).catch(() => ({}));
  if (fromPrimary?.[key] !== undefined) return fromPrimary[key];
  if (primary !== browserApi.storage.local) {
    const fromLocal = await browserApi.storage.local.get(key).catch(() => ({}));
    if (fromLocal?.[key] !== undefined) return fromLocal[key];
  }
  return undefined;
}

async function clearPendingKey(key) {
  const primary = pendingStorage();
  await primary.remove(key).catch(() => {});
  if (primary !== browserApi.storage.local) {
    await browserApi.storage.local.remove(key).catch(() => {});
  }
}

async function sweepStalePending() {
  await withPendingLock(async () => {
    // Remove any legacy pendingSend left by an older extension version.
    await clearPendingKey("pendingSend");
    const value = await readPendingKey("pendingAutoCapture");
    if (value !== undefined && !isFreshPending(value)) {
      await clearPendingKey("pendingAutoCapture");
    }
  });
}

// MV3 context menus persist across service-worker wakes; recreating them at
// top level would wipe menus mid-interaction and race with onInstalled.
browserApi.runtime.onInstalled.addListener(() => void createMenus());
browserApi.runtime.onStartup.addListener(() => void createMenus());
void sweepStalePending().catch(() => {});
void watchDownloads();

browserApi.contextMenus.onClicked.addListener((info, tab) => {
  if (info.menuItemId === MENU_SEND_LINK && info.linkUrl) {
    void sendLink(info.linkUrl, tab).catch(() => {});
  } else if (info.menuItemId === MENU_SEND_MEDIA) {
    // A video element's source is often a blob: stream the page builds; the
    // page itself is what Fetchpath's media engine understands.
    const source = policy.parseHttpUrl(info.srcUrl ?? "") ? info.srcUrl : tab?.url;
    if (source) void sendLink(source, tab).catch(() => {});
  } else if (info.menuItemId === MENU_SEND_PAGE && tab?.url) {
    void sendLink(tab.url, tab, { pageTitle: tab.title ?? "" }).catch(() => {});
  } else if (info.menuItemId === MENU_EXCLUDE_SITE && tab?.url) {
    void setOriginExcluded(tab.url, true).catch(() => {});
  } else if (info.menuItemId === MENU_INCLUDE_SITE && tab?.url) {
    void setOriginExcluded(tab.url, false).catch(() => {});
  }
});

async function autoCaptureGranted(permissions) {
  const grantedPerms = Array.isArray(permissions?.permissions) ? permissions.permissions : [];
  const grantedOrigins = Array.isArray(permissions?.origins) ? permissions.origins : [];
  const relevant =
    grantedPerms.includes("downloads") ||
    grantedOrigins.some((pattern) => pattern === "http://*/*" || pattern === "https://*/*" || pattern === "<all_urls>");
  if (!relevant) return false;
  return Boolean(await browserApi.permissions.contains(AUTO_CAPTURE_PERMISSIONS).catch(() => false));
}

// Turning on automatic capture opens a browser permission prompt that can close
// the popup before it finishes; complete enabling once the grant arrives.
browserApi.permissions.onAdded?.addListener(async (permissions) => {
  void watchDownloads();
  await withPendingLock(async () => {
    const pendingAutoCapture = await readPendingKey("pendingAutoCapture");
    if (pendingAutoCapture !== undefined) {
      if (!isFreshPending(pendingAutoCapture)) {
        await clearPendingKey("pendingAutoCapture");
      } else if (await autoCaptureGranted(permissions)) {
        await clearPendingKey("pendingAutoCapture");
        await setAutoCapture(true);
        await setBadge("✓", "#19745f");
      }
    }
  });
});

function handleMessage(message, sender) {
  if (message?.type === "get_state") return readState();
  if (message?.type === "probe") return probe();
  if (message?.type === "send_link" && message.url) {
    const pageTitle = typeof message.pageTitle === "string" ? message.pageTitle : undefined;
    const captureId = typeof message.captureId === "string" && message.captureId ? message.captureId : undefined;
    return sendLink(message.url, sender.tab ?? { url: message.url }, { pageTitle, captureId });
  }
  if (message?.type === "set_auto_capture") {
    const clear = message.claimPending ? withPendingLock(() => clearPendingKey("pendingAutoCapture")) : Promise.resolve();
    return clear.then(() => setAutoCapture(Boolean(message.enabled)));
  }
  if (message?.type === "set_excluded" && message.url) return setOriginExcluded(message.url, Boolean(message.excluded));
  return undefined;
}

browserApi.runtime.onMessage.addListener((message, sender, sendResponse) => {
  const result = handleMessage(message, sender);
  if (!result) return undefined;
  // Support both Chromium callback-based sendResponse and Promise-based callers.
  if (typeof sendResponse === "function" && !globalThis.browser) {
    Promise.resolve(result).then(sendResponse, (error) =>
      sendResponse({ accepted: false, reason: error?.message ?? "capture_failed" }),
    );
    return true;
  }
  return result;
});
