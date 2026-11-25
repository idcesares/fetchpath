const api = globalThis.browser ?? globalThis.chrome;

const statusText = document.getElementById("status");
const statusHelp = document.getElementById("status-help");
const sendPage = document.getElementById("send-page");
const siteCookiesRow = document.getElementById("site-cookies-row");
const siteCookies = document.getElementById("site-cookies");
const siteCookiesLabel = document.getElementById("site-cookies-label");
const siteCookiesHelp = document.getElementById("site-cookies-help");
const autoCapture = document.getElementById("auto-capture");
const result = document.getElementById("result");
const recentSection = document.getElementById("recent-section");
const recent = document.getElementById("recent");

const REASONS = {
  native_host_unavailable: "Fetchpath isn't installed on this computer, or couldn't be started.",
  native_host_timeout: "Fetchpath took too long to answer. Try again once it is running.",
  capture_failed: "Fetchpath couldn't take this download.",
  site_permission_denied: "Fetchpath needs permission for this site to pass on your sign-in.",
  site_excluded: "This site is excluded. Right-click the page and choose Allow this site in Fetchpath.",
  unsupported_scheme: "Only http and https pages can be sent.",
  "bridge.invalid_filename": "This page's file name could not be saved safely on Windows.",
  "bridge.invalid_cookie": "One of the site's cookies could not be passed to Fetchpath.",
  "bridge.cookie_scope_mismatch": "One of the site's cookies did not match this address.",
  "bridge.too_many_cookies": "This site sent more cookies than Fetchpath accepts at once.",
  "bridge.unsupported_method": "Only standard GET downloads can be sent to Fetchpath.",
  "bridge.unsupported_scheme": "Only http and https pages can be sent.",
  "bridge.invalid_url": "This link isn't a valid web address.",
  "bridge.url_credentials_rejected": "Links with an embedded username or password can't be sent.",
  "bridge.invalid_referrer": "This page's referrer address wasn't valid.",
  "bridge.explicit_action_required": "Start this send from the Fetchpath menu or button.",
  "contract.idempotency_conflict": "This link collided with an earlier capture. Try sending it again.",
  "contract.unsupported_version": "The Fetchpath extension and desktop app are out of sync. Reinstall Fetchpath.",
};

let currentTab = null;
let currentOriginPattern = null;

function pendingStorage() {
  return api.storage?.session ?? api.storage.local;
}

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
  currentTab = tab ?? null;
  const tabUrl = httpUrl(tab?.url ?? "");
  currentOriginPattern = tabUrl ? `${tabUrl.protocol}//${tabUrl.hostname}/*` : null;
  const hasSiteCookies = currentOriginPattern
    ? Boolean(await api.permissions.contains({ origins: [currentOriginPattern] }).catch(() => false))
    : false;
  const connected = connection?.connected === true;
  statusText.dataset.state = connected ? "ok" : "bad";
  statusText.textContent = connected ? "Connected to Fetchpath" : "Fetchpath isn't answering";
  statusHelp.hidden = connected;
  sendPage.disabled = !tabUrl;
  siteCookiesRow.hidden = !tabUrl;
  siteCookiesHelp.hidden = !tabUrl;
  if (tabUrl) {
    siteCookiesLabel.textContent = `Share sign-in cookies for ${tabUrl.hostname}`;
    siteCookies.checked = hasSiteCookies;
  }
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
    const tab = (await activeTab()) ?? currentTab;
    const url = httpUrl(tab?.url ?? "");
    if (!url) {
      show(REASONS.unsupported_scheme, false);
      return;
    }
    const response = await api.runtime.sendMessage({
      type: "send_link",
      url: tab.url,
      pageTitle: tab.title ?? "",
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

siteCookies.addEventListener("change", async () => {
  if (!currentOriginPattern) return;
  const origins = [currentOriginPattern];
  if (siteCookies.checked) {
    const granted = await api.permissions.request({ origins }).catch(() => false);
    if (!granted) {
      siteCookies.checked = false;
      show("Sign-in cookies were not shared for this site. Public links still work.", false);
      return;
    }
    show("Fetchpath can now include your sign-in cookies for this site.", true);
  } else {
    await api.permissions.remove({ origins }).catch(() => false);
    const stillGranted = Boolean(await api.permissions.contains({ origins }).catch(() => false));
    if (stillGranted) {
      siteCookies.checked = true;
      show(
        autoCapture.checked
          ? "Automatic download sending already grants access to all sites. Turn it off first to revoke one site."
          : "All-sites access is still granted in your browser's extension settings.",
        false,
      );
      return;
    }
    show("Sign-in cookies will no longer be shared for this site.", true);
  }
});

autoCapture.addEventListener("change", async () => {
  if (autoCapture.checked) {
    // Chrome's permission prompt can close the popup before set_auto_capture
    // runs; record pendingAutoCapture synchronously so background.js finishes
    // enabling automatic capture when the user clicks Allow.
    const pendingWrite = pendingStorage().set({ pendingAutoCapture: { at: Date.now() } });
    const granted = await api.permissions
      .request({
        permissions: ["downloads"],
        origins: ["http://*/*", "https://*/*"],
      })
      .catch(() => false);
    await pendingWrite.catch(() => {});
    if (!granted) {
      await pendingStorage().remove("pendingAutoCapture").catch(() => {});
      autoCapture.checked = false;
      show("Automatic sending needs permission to see downloads and read sign-ins for the sites they come from.", false);
      return;
    }
  }
  await api.runtime.sendMessage({
    type: "set_auto_capture",
    enabled: autoCapture.checked,
    claimPending: true,
  });
  const enabled = autoCapture.checked;
  await render().catch(() => {});
  show(enabled ? "Downloads will go to Fetchpath." : "Downloads will stay in the browser.", true);
});

void render();
