const browserApi = globalThis.browser ?? globalThis.chrome;
const HOST_NAME = "com.fetchpath.browser_spike";
const observations = new Map();

function redactUrl(rawUrl) {
  try {
    const url = new URL(rawUrl);
    url.search = "";
    url.hash = "";
    return url.toString();
  } catch {
    return "<invalid-url>";
  }
}

function remember(details) {
  const existing = observations.get(details.url) ?? {};
  observations.set(details.url, {
    ...existing,
    method: details.method ?? existing.method ?? "GET",
    requestId: details.requestId ?? existing.requestId,
    hasRequestBody: Boolean(details.requestBody) || Boolean(existing.hasRequestBody),
    observedAt: Date.now(),
  });
}

browserApi.webRequest.onBeforeRequest.addListener(
  remember,
  { urls: ["http://127.0.0.1/*"] },
  ["requestBody"],
);

function rememberHeaders(details) {
  remember(details);
  const sensitiveNames = new Set(["authorization", "cookie", "proxy-authorization"]);
  const hasAuthContext = (details.requestHeaders ?? []).some((header) =>
    sensitiveNames.has(header.name.toLowerCase()),
  );
  const existing = observations.get(details.url) ?? {};
  observations.set(details.url, { ...existing, hasAuthContext, headersObserved: true });
}

async function waitForObservation(rawUrl) {
  const deadline = Date.now() + 250;
  while (Date.now() < deadline) {
    const observation = observations.get(rawUrl);
    if (observation?.headersObserved) return observation;
    await new Promise((resolve) => setTimeout(resolve, 10));
  }
  return observations.get(rawUrl) ?? {};
}

try {
  browserApi.webRequest.onBeforeSendHeaders.addListener(
    rememberHeaders,
    { urls: ["http://127.0.0.1/*"] },
    ["requestHeaders", "extraHeaders"],
  );
} catch {
  browserApi.webRequest.onBeforeSendHeaders.addListener(
    rememberHeaders,
    { urls: ["http://127.0.0.1/*"] },
    ["requestHeaders"],
  );
}

async function readEvents() {
  const state = await browserApi.storage.local.get("events");
  return state.events ?? [];
}

async function recordEvent(event) {
  const events = await readEvents();
  events.push(event);
  await browserApi.storage.local.set({ events: events.slice(-50) });
}

async function captureDownload(item) {
  const rawUrl = item.finalUrl || item.url;
  const parsed = (() => {
    try {
      return new URL(rawUrl);
    } catch {
      return null;
    }
  })();
  const observation = parsed && ["http:", "https:"].includes(parsed.protocol)
    ? await waitForObservation(rawUrl)
    : {};
  const scopedCookies = parsed && ["http:", "https:"].includes(parsed.protocol)
    ? await browserApi.cookies.getAll({ url: rawUrl })
    : [];
  const captureId = `browser-download-${item.id}`;
  const request = {
    schema_version: 1,
    type: "capture",
    capture_id: captureId,
    method: observation.method ?? "GET",
    url: rawUrl,
    has_auth_context: Boolean(observation.hasAuthContext) || scopedCookies.length > 0,
    observation_complete: Boolean(observation.headersObserved),
    has_request_body: Boolean(observation.hasRequestBody),
    signed_hint: Boolean(parsed && [...parsed.searchParams.keys()].some((key) =>
      /^(token|sig|signature|expires|x-amz-signature)$/i.test(key),
    )),
  };

  let response;
  try {
    response = await browserApi.runtime.sendNativeMessage(HOST_NAME, request);
  } catch (error) {
    await recordEvent({
      capture_id: captureId,
      browser_download_id: item.id,
      method: request.method,
      redacted_url: redactUrl(rawUrl),
      accepted: false,
      fallback: true,
      reason: `native_host_error:${error.message}`,
    });
    return;
  }

  let cancelObserved = false;
  if (response?.accepted === true) {
    await browserApi.downloads.cancel(item.id);
    const [afterCancel] = await browserApi.downloads.search({ id: item.id });
    cancelObserved = afterCancel?.state === "interrupted";
  }

  await recordEvent({
    capture_id: captureId,
    browser_download_id: item.id,
    method: request.method,
    redacted_url: redactUrl(rawUrl),
    auth_context_observed: request.has_auth_context,
    signed_hint: request.signed_hint,
    accepted: response?.accepted === true,
    deduplicated: response?.deduplicated === true,
    reason: response?.reason ?? null,
    cancel_observed: cancelObserved,
    fallback: response?.accepted !== true,
  });
}

browserApi.downloads.onCreated.addListener((item) => {
  void captureDownload(item).catch((error) => recordEvent({
    browser_download_id: item.id,
    redacted_url: redactUrl(item.finalUrl || item.url),
    accepted: false,
    fallback: true,
    reason: `extension_internal_error:${error.message}`,
  }));
});

browserApi.runtime.onMessage.addListener((message) => {
  if (message?.type === "probe") {
    return browserApi.runtime.sendNativeMessage(HOST_NAME, {
      schema_version: 1,
      type: "probe",
    });
  }
  if (message?.type === "get_events") {
    return readEvents();
  }
  if (message?.type === "clear_events") {
    return browserApi.storage.local.set({ events: [] }).then(() => ({ cleared: true }));
  }
  if (message?.type === "direct_capture_for_test") {
    return browserApi.runtime.sendNativeMessage(HOST_NAME, message.request);
  }
  return undefined;
});
