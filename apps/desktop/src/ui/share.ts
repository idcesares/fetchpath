import { invoke } from "@tauri-apps/api/core";
import { open, save } from "@tauri-apps/plugin-dialog";
import { required } from "./dom";
import { formatBytes } from "./format";

/** Public share links and their setup, as the engine reports them (never a link). */
export interface ShareItem {
  handle: string;
  test: boolean;
  name: string;
  size: number;
  state: "live" | "completed" | "revoked" | "expired" | "changed" | "over_budget" | "unknown";
  expires_in_seconds: number;
  opened: number;
  delivered: number;
  total: number;
}

export interface ShareView {
  enabled: boolean;
  base_url?: string;
  port: number;
  tested: boolean;
  serving?: string;
  problem?: string;
  relay: boolean;
  relay_key_stored: boolean;
  shares: ShareItem[];
}

/** A link just made: the only time it is available outside the engine. */
export interface NewShare {
  link: string;
  share: ShareItem;
  qr_rows: string[];
}

export interface SharePanelContext {
  settingsDialog: HTMLDialogElement;
  announce(message: string): void;
  showError(error: unknown): void;
  clearError(): void;
  setStatus(message: string): void;
}

export interface SharePanel {
  refresh(): Promise<void>;
  onDialogClose(): void;
}

const SHARE_STATE_LABEL: Record<ShareItem["state"], string> = {
  live: "waiting to be used",
  completed: "used up",
  revoked: "revoked",
  expired: "expired",
  changed: "stopped, the file changed",
  over_budget: "stopped, too many requests",
  unknown: "unknown",
};

function shareDuration(seconds: number): string {
  if (seconds >= 86400) return `${Math.round(seconds / 86400)} d`;
  if (seconds >= 3600) return `${Math.round(seconds / 3600)} h`;
  return `${Math.max(1, Math.round(seconds / 60))} min`;
}

/** Public file sharing, tunnel/relay configuration and QR export inside Settings. */
export function createSharePanel(ctx: SharePanelContext): SharePanel {
  const shareUrl = required<HTMLInputElement>("share-url");
  const sharePort = required<HTMLInputElement>("share-port");
  const shareSave = required<HTMLButtonElement>("share-save");
  const shareRelayKey = required<HTMLInputElement>("share-relay-key");
  const shareRelayOn = required<HTMLButtonElement>("share-relay-on");
  const shareRelayOff = required<HTMLButtonElement>("share-relay-off");
  const shareState = required<HTMLParagraphElement>("share-state");
  const shareTest = required<HTMLButtonElement>("share-test");
  const shareNew = required<HTMLDivElement>("share-new");
  const shareNewText = required<HTMLParagraphElement>("share-new-text");
  const shareNewLink = required<HTMLInputElement>("share-new-link");
  const shareCopy = required<HTMLButtonElement>("share-copy");
  const shareHide = required<HTMLButtonElement>("share-hide");
  const shareQr = document.getElementById("share-qr") as unknown as SVGSVGElement;
  const shareQrNote = required<HTMLParagraphElement>("share-qr-note");
  const shareSaveQr = required<HTMLButtonElement>("share-save-qr");
  const shareEnabled = required<HTMLInputElement>("share-enabled");
  const shareEnabledState = required<HTMLParagraphElement>("share-enabled-state");
  const shareExpiry = required<HTMLSelectElement>("share-expiry");
  const shareFile = required<HTMLButtonElement>("share-file");
  const shareRevokeAll = required<HTMLButtonElement>("share-revoke-all");
  const shareList = required<HTMLUListElement>("share-list");

  let shareNewHandle: string | null = null;
  let sharePoll: number | undefined;
  let shareWasTested = false;
  /** True while the link on show is the setup test link. */
  let shareShowingTest = false;

  function stopPolling(): void {
    if (sharePoll !== undefined) {
      window.clearInterval(sharePoll);
      sharePoll = undefined;
    }
  }

  /** Forgets the link and its code. They exist only in this page until hidden. */
  function hideNewShare(): void {
    shareNewHandle = null;
    shareNew.hidden = true;
    shareNewLink.value = "";
    shareQr.replaceChildren();
    shareQr.toggleAttribute("hidden", true);
    shareQrNote.hidden = true;
    shareSaveQr.hidden = true;
  }

  function showNewShare(made: NewShare, text: string): void {
    shareNewHandle = made.share.handle;
    shareNewText.textContent = text;
    shareNewLink.value = made.link;
    shareNew.hidden = false;
    const size = made.qr_rows.length;
    shareQr.replaceChildren();
    shareQr.toggleAttribute("hidden", size === 0);
    shareQrNote.hidden = size === 0;
    shareSaveQr.hidden = size === 0;
    if (!size) return;
    // Dark modules on a white square with a 4-module margin, whatever the theme:
    // scanners need that contrast.
    const ns = "http://www.w3.org/2000/svg";
    shareQr.setAttribute("viewBox", `-4 -4 ${size + 8} ${size + 8}`);
    shareQr.setAttribute("shape-rendering", "crispEdges");
    const ground = document.createElementNS(ns, "rect");
    ground.setAttribute("x", "-4");
    ground.setAttribute("y", "-4");
    ground.setAttribute("width", String(size + 8));
    ground.setAttribute("height", String(size + 8));
    ground.setAttribute("fill", "#fff");
    const modules = document.createElementNS(ns, "path");
    let path = "";
    made.qr_rows.forEach((row, y) => {
      for (let x = 0; x < row.length; x++) if (row[x] === "1") path += `M${x} ${y}h1v1h-1z`;
    });
    modules.setAttribute("d", path);
    modules.setAttribute("fill", "#000");
    shareQr.append(ground, modules);
  }

  function focusFallback(): void {
    if (!shareFile.disabled) shareFile.focus({ preventScroll: true });
    else if (!shareTest.disabled) shareTest.focus({ preventScroll: true });
    else shareUrl.focus({ preventScroll: true });
  }

  function render(view: ShareView): void {
    if (document.activeElement !== shareUrl) shareUrl.value = view.base_url ?? "";
    if (document.activeElement !== sharePort) sharePort.value = String(view.port);
    const listening = Boolean(view.serving);
    shareState.textContent = !view.base_url
      ? "No address is saved yet."
      : view.relay
        ? listening
          ? `Enrolled with the relay at ${view.base_url}.`
          : `Not enrolled with the relay: ${view.problem ?? "the engine has not connected yet."}`
        : listening
          ? `Listening on ${view.serving}. Your tunnel must forward ${view.base_url} to this address.`
          : `Not listening: ${view.problem ?? "the engine has not started it yet."}`;
    shareRelayOn.disabled = !view.base_url;
    shareRelayOff.disabled = !view.relay;
    shareRelayKey.placeholder = view.relay_key_stored ? "A key is stored; leave empty to reuse it" : "";
    shareTest.disabled = !listening;
    shareEnabled.checked = view.enabled;
    // Always allow turning sharing off while enabled, even if the relay/listener
    // is currently disconnected, so live links and file locks can be revoked.
    shareEnabled.disabled = !view.enabled && (!view.tested || !listening);
    shareEnabledState.textContent = view.enabled
      ? "On. Each link you make works once and for a limited time. Turn this off to stop all of them."
      : view.tested
        ? "Off. Turn it on to allow links to files."
        : "Off until a test link has been downloaded through your address.";
    shareFile.disabled = !view.enabled || !listening;
    shareRevokeAll.disabled = !view.shares.some((item) => item.state === "live");
    if (shareNewHandle && !shareShowingTest) {
      const shown = view.shares.find((item) => item.handle === shareNewHandle);
      if (!shown || shown.state !== "live") hideNewShare();
    }
    const focusedInList = shareList.contains(document.activeElement);
    shareList.replaceChildren();
    for (const item of view.shares.filter((candidate) => !candidate.test)) {
      const row = document.createElement("li");
      const label = document.createElement("span");
      const expiry = item.state === "live" ? `, expires in ${shareDuration(item.expires_in_seconds)}` : "";
      label.textContent = `${item.name}, ${formatBytes(item.size)}: ${SHARE_STATE_LABEL[item.state]}${expiry}`;
      row.append(label);
      if (item.state === "live") {
        const revoke = document.createElement("button");
        revoke.type = "button";
        revoke.className = "quiet";
        revoke.textContent = "Revoke";
        revoke.setAttribute("aria-label", `Revoke the link to ${item.name}`);
        revoke.addEventListener("click", async () => {
          ctx.clearError();
          try {
            if (shareNewHandle === item.handle) hideNewShare();
            render(await invoke<ShareView>("revoke_share", { handle: item.handle }));
            ctx.announce(`Revoked the link to ${item.name}.`);
            focusFallback();
          } catch (error) {
            ctx.showError(error);
          }
        });
        row.append(revoke);
      }
      shareList.append(row);
    }
    if (focusedInList && !shareList.contains(document.activeElement)) focusFallback();
    // The setup test counts when the engine saw it delivered in order.
    if (view.tested && !shareWasTested && shareShowingTest && !shareNew.hidden) {
      shareNewText.textContent = "The test link was downloaded through your address. You can now allow public file links.";
      ctx.announce(shareNewText.textContent);
    }
    shareWasTested = view.tested;
    const waitingForTest = !view.tested && view.shares.some((item) => item.test && item.state === "live");
    const connecting = Boolean(view.base_url) && !listening && !view.problem;
    const shouldPoll = ctx.settingsDialog.open && (waitingForTest || connecting);
    if (shouldPoll && sharePoll === undefined) {
      sharePoll = window.setInterval(() => void refresh(), 1000);
    } else if (!shouldPoll) {
      stopPolling();
    }
  }

  async function refresh(): Promise<void> {
    if (!ctx.settingsDialog.open) {
      stopPolling();
      return;
    }
    try {
      render(await invoke<ShareView>("share_status"));
    } catch (error) {
      shareState.textContent = "Public links can't be shown right now.";
      stopPolling();
      ctx.showError(error);
    }
  }

  shareSave.addEventListener("click", async () => {
    ctx.clearError();
    const port = sharePort.value.trim() ? Number(sharePort.value) : null;
    if (port !== null && (!Number.isInteger(port) || port < 1024 || port > 65535)) {
      ctx.showError("Enter a port between 1024 and 65535.");
      return;
    }
    const baseUrl = shareUrl.value.trim();
    try {
      const view = await invoke<ShareView>("configure_sharing", { baseUrl, port });
      hideNewShare();
      shareWasTested = false;
      render(view);
      window.setTimeout(() => void refresh(), 1000);
      ctx.announce(baseUrl ? "Address saved. Make a test link to check that it works." : "Address cleared.");
    } catch (error) {
      ctx.showError(error);
      await refresh();
    }
  });

  shareRelayOn.addEventListener("click", async () => {
    ctx.clearError();
    // The key leaves the field at once and is held nowhere else.
    const key = shareRelayKey.value.trim();
    shareRelayKey.value = "";
    try {
      const view = await invoke<ShareView>("configure_relay", { relay: true, key: key || null });
      hideNewShare();
      shareWasTested = false;
      render(view);
      window.setTimeout(() => void refresh(), 1000);
      ctx.announce("Relay saved. Waiting for enrollment…");
    } catch (error) {
      ctx.showError(error);
      await refresh();
    }
  });

  shareRelayOff.addEventListener("click", async () => {
    ctx.clearError();
    shareRelayKey.value = "";
    try {
      const view = await invoke<ShareView>("configure_relay", { relay: false, key: null });
      hideNewShare();
      shareWasTested = false;
      render(view);
      window.setTimeout(() => void refresh(), 1000);
      ctx.announce("Back to a tunnel. Make a test link to check that it works.");
    } catch (error) {
      ctx.showError(error);
      await refresh();
    }
  });

  shareTest.addEventListener("click", async () => {
    ctx.clearError();
    try {
      const made = await invoke<NewShare>("start_share_test");
      shareShowingTest = true;
      showNewShare(made, "Open this link on a phone with Wi-Fi off, or scan the code, and press Download. This page shows when it has worked.");
      ctx.announce("Test link ready.");
      await refresh();
    } catch (error) {
      ctx.showError(error);
    }
  });

  shareCopy.addEventListener("click", async () => {
    try {
      await navigator.clipboard.writeText(shareNewLink.value);
      ctx.setStatus("Link copied.");
    } catch {
      shareNewLink.select();
      ctx.setStatus("Select the link and copy it.");
    }
  });

  /** Saves the code as an image to a place the person picks. The image is the link. */
  shareSaveQr.addEventListener("click", async () => {
    ctx.clearError();
    try {
      const chosen = await save({
        title: "Save the QR code as an image",
        defaultPath: "fetchpath-link.png",
        filters: [
          { name: "PNG image", extensions: ["png"] },
          { name: "SVG image", extensions: ["svg"] },
        ],
      });
      if (typeof chosen !== "string") return;
      await invoke("save_share_qr", { path: chosen, link: shareNewLink.value });
      ctx.setStatus("QR image saved. It holds the whole link: delete it after sending.");
    } catch (error) {
      ctx.showError(error);
    }
  });

  shareHide.addEventListener("click", () => {
    hideNewShare();
    focusFallback();
  });

  shareEnabled.addEventListener("change", async () => {
    ctx.clearError();
    try {
      const view = await invoke<ShareView>("set_sharing_enabled", { enabled: shareEnabled.checked });
      if (!view.enabled && !shareShowingTest) hideNewShare();
      render(view);
      ctx.announce(shareEnabledState.textContent ?? "");
    } catch (error) {
      ctx.showError(error);
      await refresh();
    }
  });

  shareFile.addEventListener("click", async () => {
    ctx.clearError();
    const label = shareFile.textContent;
    shareFile.disabled = true;
    try {
      const selected = await open({ directory: false, multiple: false, title: "Choose a file to share with a link" });
      if (typeof selected !== "string") return;
      shareFile.textContent = "Making link…";
      const made = await invoke<NewShare>("create_share", {
        path: selected,
        expiresInSeconds: Number(shareExpiry.value),
      });
      shareShowingTest = false;
      showNewShare(made, `Link to ${made.share.name}. It works once and cannot be shown again, so copy it now. The file stays locked while the link lives.`);
      ctx.announce("Link made. Copy it now: it is shown only once.");
    } catch (error) {
      ctx.showError(error);
    } finally {
      shareFile.textContent = label;
      await refresh();
    }
  });

  shareRevokeAll.addEventListener("click", async () => {
    ctx.clearError();
    try {
      hideNewShare();
      render(await invoke<ShareView>("revoke_share", { handle: null }));
      ctx.announce("Every link is revoked.");
      focusFallback();
    } catch (error) {
      ctx.showError(error);
    }
  });

  function onDialogClose(): void {
    stopPolling();
    shareRelayKey.value = "";
    hideNewShare();
  }

  return { refresh, onDialogClose };
}
