const api = globalThis.browser ?? globalThis.chrome;

const statusText = document.getElementById("status");
const statusHelp = document.getElementById("status-help");
const sendPage = document.getElementById("send-page");
const autoCapture = document.getElementById("auto-capture");
const result = document.getElementById("result");
const recentSection = document.getElementById("recent-section");
const recent = document.getElementById("recent");

const REASONS = {
  native_host_unavailable: "Fetchpath isn't installed on this computer, or couldn't be started.",
  site_permission_denied: "Fetchpath needs permission for this site to pass on your sign-in.",
  site_excluded: "This site is excluded. Right-click the page and choose Allow this site in Fetchpath.",
  unsupported_scheme: "Only http and https pages can be sent.",
};

function show(message, ok) {
  result.textContent = message;
  result.dataset.state = ok ? "ok" : "bad";
  result.hidden = false;
}

async function activeTab() {
  const [tab] = await api.tabs.query({ active: true, currentWindow: true });
  return tab;
}

function httpUrl(raw) {
  try {
    const url = new URL(raw);
    return ["http:", "https:"].includes(url.protocol) ? url : null;
  } catch {
    return null;
  }
}

async function render() {
  const [connection, state, tab] = await Promise.all([
    api.runtime.sendMessage({ type: "probe" }),
    api.runtime.sendMessage({ type: "get_state" }),
    activeTab(),
  ]);
  const connected = connection?.connected === true;
  statusText.dataset.state = connected ? "ok" : "bad";
  statusText.textContent = connected ? "Connected to Fetchpath" : "Fetchpath isn't answering";
  statusHelp.hidden = connected;
  sendPage.disabled = !httpUrl(tab?.url ?? "");
  autoCapture.checked = state?.autoCapture === true;

  const events = (state?.events ?? []).filter((event) => event.redactedUrl).slice(-5).reverse();
  recentSection.hidden = events.length === 0;
  recent.replaceChildren(
    ...events.map((event) => {
      const item = document.createElement("li");
      item.textContent = `${event.accepted ? "✓" : "✕"} ${event.redactedUrl}`;
      item.title = event.redactedUrl;
      return item;
    }),
  );
}

sendPage.addEventListener("click", async () => {
  sendPage.disabled = true;
  show("Sending…", true);
  try {
    const tab = await activeTab();
    const url = httpUrl(tab?.url ?? "");
    if (!url) {
      show(REASONS.unsupported_scheme, false);
      return;
    }
    const origins = [`${url.protocol}//${url.hostname}/*`];
    // Requested here, inside the click: a request made by the background script
    // after this message would no longer count as the person's action.
    if (!(await api.permissions.contains({ origins }))) {
      // The browser's permission prompt can close this popup before the answer
      // arrives. The background script finishes the send when it is granted.
      await api.storage.local.set({ pendingSend: { url: tab.url, pageTitle: tab.title ?? "", at: Date.now() } });
      if (!(await api.permissions.request({ origins }))) {
        await api.storage.local.remove("pendingSend");
        show(REASONS.site_permission_denied, false);
        return;
      }
    }
    const response = await api.runtime.sendMessage({
      type: "send_link",
      url: tab.url,
      pageTitle: tab.title ?? "",
      claimPending: true,
    });
    if (response?.accepted) show("Sent. Fetchpath will open with it.", true);
    else show(REASONS[response?.reason] ?? `Fetchpath couldn't take it (${response?.reason ?? "unknown"}).`, false);
  } catch (error) {
    show(`Fetchpath couldn't send this page (${error?.message ?? "unknown"}).`, false);
  } finally {
    sendPage.disabled = false;
  }
  await render().catch(() => {});
});

autoCapture.addEventListener("change", async () => {
  if (autoCapture.checked) {
    const granted = await api.permissions.request({
      permissions: ["downloads"],
      origins: ["http://*/*", "https://*/*"],
    });
    if (!granted) {
      autoCapture.checked = false;
      show("Automatic sending needs permission to see downloads and read sign-ins for the sites they come from.", false);
      return;
    }
  }
  await api.runtime.sendMessage({ type: "set_auto_capture", enabled: autoCapture.checked });
  show(autoCapture.checked ? "Downloads will go to Fetchpath." : "Downloads will stay in the browser.", true);
});

void render();
