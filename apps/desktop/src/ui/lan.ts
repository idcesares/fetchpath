import { invoke } from "@tauri-apps/api/core";
import { required } from "./dom";

/** Paired computers and sharing, as the engine reports them. */
export interface PairedDevice {
  key: string;
  fingerprint: string;
  label: string;
}

export interface LanView {
  sharing: boolean;
  serving?: string;
  problem?: string;
  fingerprint: string;
  devices: PairedDevice[];
  pairing?: {
    state: "waiting" | "paired" | "failed" | "expired" | "cancelled" | "unknown";
    code?: string;
    address: string;
    expires_at: string;
    device?: PairedDevice;
    problem?: string;
  };
}

export interface LanPanelContext {
  announce(message: string): void;
  showError(error: unknown): void;
  clearError(): void;
  setStatus(message: string): void;
  errorMessage(error: unknown): string;
}

export interface LanPanel {
  refresh(): Promise<void>;
  onDialogClose(): void;
}

function pairingText(pairing: NonNullable<LanView["pairing"]>, own: string): string {
  switch (pairing.state) {
    case "waiting": {
      const seconds = Math.max(0, Math.round((Date.parse(pairing.expires_at) - Date.now()) / 1000));
      return `On the other computer, choose Pair with a computer that shows a code, and enter the address ${pairing.address} and this code. It works once, for ${seconds} more seconds. The other computer should show this computer's fingerprint, ${own}.`;
    }
    case "paired":
      return `Paired with a computer whose fingerprint is ${pairing.device?.fingerprint ?? "unknown"}. Check that the other computer shows the same, and that it shows ${own} for this one.`;
    case "expired":
      return "The code expired before another computer used it. Show a new one to try again.";
    case "failed":
      return pairing.problem ?? "Pairing failed. Nothing was paired.";
    default:
      return "";
  }
}

/** Paired LAN sharing and device pairing controls inside Settings. */
export function createLanPanel(ctx: LanPanelContext): LanPanel {
  const lanSelf = required<HTMLParagraphElement>("lan-self");
  const lanSharing = required<HTMLInputElement>("lan-sharing");
  const lanSharingState = required<HTMLParagraphElement>("lan-sharing-state");
  const lanDevices = required<HTMLUListElement>("lan-devices");
  const lanPair = required<HTMLButtonElement>("lan-pair");
  const lanPairCancel = required<HTMLButtonElement>("lan-pair-cancel");
  const lanPairing = required<HTMLDivElement>("lan-pairing");
  const lanPairingText = required<HTMLParagraphElement>("lan-pairing-text");
  const lanCode = required<HTMLParagraphElement>("lan-code");
  const lanJoinAddress = required<HTMLInputElement>("lan-join-address");
  const lanJoinCode = required<HTMLInputElement>("lan-join-code");
  const lanJoinName = required<HTMLInputElement>("lan-join-name");
  const lanJoin = required<HTMLButtonElement>("lan-join");
  const lanJoinResult = required<HTMLParagraphElement>("lan-join-result");

  let lanPoll: number | undefined;
  let lanPairingState: string | undefined;

  function stopPolling(): void {
    if (lanPoll !== undefined) {
      window.clearInterval(lanPoll);
      lanPoll = undefined;
    }
  }

  function render(lan: LanView): void {
    lanSelf.textContent = `This computer's fingerprint: ${lan.fingerprint}`;
    lanSharing.checked = lan.sharing;
    lanSharingState.textContent = !lan.sharing
      ? "Off. Nothing is offered to any computer."
      : lan.serving
        ? `On. Paired computers reach this one at ${lan.serving}. Fetchpath keeps running in the background while sharing is on.`
        : `On, but not running: ${lan.problem ?? "the engine is not sharing."}`;

    lanDevices.replaceChildren();
    if (!lan.devices.length) {
      const empty = document.createElement("li");
      empty.className = "field-note";
      empty.textContent = "No paired computers yet.";
      lanDevices.append(empty);
    }
    for (const device of lan.devices) {
      const item = document.createElement("li");
      const text = document.createElement("span");
      text.textContent = `${device.label} · fingerprint ${device.fingerprint}`;
      const remove = document.createElement("button");
      remove.type = "button";
      remove.className = "secondary danger";
      remove.textContent = "Remove";
      remove.setAttribute("aria-label", `Remove ${device.label}, fingerprint ${device.fingerprint}`);
      remove.addEventListener("click", async () => {
        ctx.clearError();
        try {
          render(await invoke<LanView>("unpair_device", { key: device.key }));
          ctx.setStatus(`${device.label} is no longer paired. It can't get files from this computer.`);
          lanPair.focus();
        } catch (error) {
          ctx.showError(error);
        }
      });
      item.append(text, remove);
      lanDevices.append(item);
    }

    const pairing = lan.pairing;
    lanPairing.hidden = !pairing || pairing.state === "cancelled";
    lanPairCancel.hidden = pairing?.state !== "waiting";
    lanCode.hidden = !pairing?.code;
    lanCode.textContent = pairing?.code ?? "";
    if (pairing) {
      lanPairingText.textContent = pairingText(pairing, lan.fingerprint);
      // Announced once per change, not on every poll.
      if (pairing.state !== lanPairingState && pairing.state !== "waiting") ctx.announce(lanPairingText.textContent);
    }
    lanPairingState = pairing?.state;
    const waiting = pairing?.state === "waiting";
    if (waiting && lanPoll === undefined) {
      lanPoll = window.setInterval(() => void refresh(), 1000);
    } else if (!waiting) {
      stopPolling();
    }
  }

  async function refresh(): Promise<void> {
    try {
      render(await invoke<LanView>("lan_status"));
    } catch (error) {
      lanSelf.textContent = "Paired computers can't be shown right now.";
      stopPolling();
      ctx.showError(error);
    }
  }

  lanSharing.addEventListener("change", async () => {
    ctx.clearError();
    try {
      render(await invoke<LanView>("set_lan_sharing", { enabled: lanSharing.checked }));
      ctx.announce(lanSharingState.textContent ?? "");
    } catch (error) {
      ctx.showError(error);
      await refresh();
    }
  });

  lanPair.addEventListener("click", async () => {
    ctx.clearError();
    try {
      render(await invoke<LanView>("start_pairing"));
      ctx.announce(`Pairing code ${lanCode.textContent}. ${lanPairingText.textContent}`);
    } catch (error) {
      ctx.showError(error);
    }
  });

  lanPairCancel.addEventListener("click", async () => {
    ctx.clearError();
    try {
      render(await invoke<LanView>("cancel_pairing"));
      ctx.setStatus("The pairing code was withdrawn.");
      lanPair.focus();
    } catch (error) {
      ctx.showError(error);
    }
  });

  lanJoin.addEventListener("click", async () => {
    ctx.clearError();
    lanJoinResult.textContent = "Pairing…";
    lanJoin.disabled = true;
    try {
      const device = await invoke<PairedDevice>("join_pairing", {
        address: lanJoinAddress.value,
        code: lanJoinCode.value,
        label: lanJoinName.value || null,
      });
      lanJoinCode.value = "";
      await refresh();
      lanJoinResult.textContent = `Paired with ${device.label}, fingerprint ${device.fingerprint}. Check that the other computer shows the same.`;
    } catch (error) {
      lanJoinResult.textContent = ctx.errorMessage(error);
    } finally {
      lanJoin.disabled = false;
    }
  });

  return { refresh, onDialogClose: stopPolling };
}
