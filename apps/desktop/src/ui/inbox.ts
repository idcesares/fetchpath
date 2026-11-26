import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { required } from "./dom";
import { formatBytes } from "./format";

/** A file an agent offered (FP-115). Every string except the notices is data. */
export interface OfferRow {
  id: string;
  state: "offered" | "copying" | "completed" | "declined" | "withdrawn" | "expired" | "failed" | "unknown";
  open: boolean;
  fileName: string;
  size: number;
  sha256: string;
  origin: string | null;
  from: string;
  savedTo: string | null;
  reason: string | null;
  hashNotice: string;
  senderNotice: string;
}

export interface InboxPanelContext {
  queueTitle: HTMLElement;
  announce(message: string): void;
  errorMessage(error: unknown): string;
}

export interface InboxPanel {
  refresh(): Promise<void>;
}

/** Files agents offered to the person (FP-115). */
export function createInboxPanel(ctx: InboxPanelContext): InboxPanel {
  const inboxPanel = required<HTMLElement>("inbox-panel");
  const inboxList = required<HTMLDivElement>("inbox-list");
  const inboxStatusRow = required<HTMLElement>("inbox-status-row");
  const inboxStatus = required<HTMLParagraphElement>("inbox-status");
  const inboxStatusDismiss = required<HTMLButtonElement>("inbox-status-dismiss");

  let inboxOffers: OfferRow[] = [];
  let inboxRenderedSignature = "";
  let inboxBusy = false;
  const trackedCopyingOfferIds = new Set<string>();

  function announceInbox(message: string): void {
    inboxStatus.textContent = message;
    inboxStatusRow.hidden = !message;
    ctx.announce(message);
  }

  function reportFinishedOffer(after: OfferRow): void {
    if (after.state === "completed") {
      announceInbox(`Saved ${after.fileName}${after.savedTo ? ` to ${after.savedTo}` : ""}.`);
    } else if (after.state === "failed") {
      announceInbox(`${after.fileName} was not saved (${after.reason ?? "failed"}).`);
    }
  }

  function render(): void {
    // Offers waiting for an answer, and those being copied. Finished ones are
    // reported once, in the status line.
    const incoming = inboxOffers.filter((offer) => offer.open && !offer.from.startsWith("user"));
    const hasStatus = Boolean(inboxStatus.textContent);
    inboxStatusRow.hidden = !hasStatus;
    inboxStatusDismiss.hidden = !hasStatus || incoming.length > 0;
    inboxPanel.hidden = incoming.length === 0 && !hasStatus;
    const signature = JSON.stringify(
      incoming.map((offer) => [offer.id, offer.state, offer.fileName, offer.size, offer.from, offer.sha256, offer.origin ?? ""]),
    );
    if (signature === inboxRenderedSignature) return;
    inboxRenderedSignature = signature;

    const active = document.activeElement;
    const focused =
      active instanceof HTMLButtonElement && inboxList.contains(active)
        ? { offerId: active.dataset.offerId, action: active.dataset.action }
        : null;

    inboxList.replaceChildren(
      ...incoming.map((offer) => {
        const card = document.createElement("section");
        card.className = "inbox-offer";
        card.setAttribute("aria-labelledby", `offer-${offer.id}`);
        const heading = document.createElement("h3");
        heading.id = `offer-${offer.id}`;
        heading.textContent = offer.fileName;
        const facts = document.createElement("p");
        facts.textContent = `${formatBytes(offer.size)}. Offered by ${offer.from}. ${offer.senderNotice}`;
        const hash = document.createElement("p");
        const code = document.createElement("code");
        code.textContent = offer.sha256;
        hash.append("SHA-256 ", code);
        const meaning = document.createElement("p");
        meaning.className = "hint";
        meaning.textContent = offer.hashNotice;
        card.append(heading, facts, hash, meaning);
        if (offer.origin) {
          const origin = document.createElement("p");
          origin.textContent = `Link of the download this file came from: ${offer.origin}`;
          card.append(origin);
        }
        const actions = document.createElement("div");
        actions.className = "agent-actions";
        const copying = offer.state === "copying";
        const accept = document.createElement("button");
        accept.type = "button";
        accept.dataset.offerId = offer.id;
        accept.dataset.action = "accept";
        accept.textContent = copying ? "Copying…" : "Accept…";
        accept.disabled = copying;
        accept.setAttribute("aria-label", `Accept ${offer.fileName}: choose a folder to copy it into`);
        accept.addEventListener("click", () => void answerOffer(offer, "accept"));
        const decline = document.createElement("button");
        decline.type = "button";
        decline.dataset.offerId = offer.id;
        decline.dataset.action = "decline";
        decline.className = "secondary";
        decline.textContent = "Decline";
        decline.disabled = copying;
        decline.setAttribute("aria-label", `Decline ${offer.fileName}`);
        decline.addEventListener("click", () => void answerOffer(offer, "decline"));
        actions.append(accept, decline);
        card.append(actions);
        return card;
      }),
    );

    if (focused?.offerId && focused.action) {
      const replacement = inboxList.querySelector<HTMLButtonElement>(
        `button[data-offer-id="${CSS.escape(focused.offerId)}"][data-action="${CSS.escape(focused.action)}"]:not([disabled])`,
      );
      const sameCard = inboxList.querySelector<HTMLButtonElement>(
        `button[data-offer-id="${CSS.escape(focused.offerId)}"]:not([disabled])`,
      );
      if (replacement) replacement.focus({ preventScroll: true });
      else if (sameCard) sameCard.focus({ preventScroll: true });
      else if (!inboxStatusDismiss.hidden) inboxStatusDismiss.focus({ preventScroll: true });
      else ctx.queueTitle.focus({ preventScroll: true });
    }
  }

  async function answerOffer(offer: OfferRow, action: "accept" | "decline"): Promise<void> {
    inboxBusy = true;
    try {
      if (action === "decline") {
        await invoke<OfferRow>("decline_offer", { offerId: offer.id });
        announceInbox(`Declined ${offer.fileName}. Nothing was copied.`);
      } else {
        const folder = await open({ directory: true, title: `Choose a folder for ${offer.fileName}` });
        if (typeof folder !== "string") return;
        const accepted = await invoke<OfferRow>("accept_offer", { offerId: offer.id, folder });
        inboxOffers = inboxOffers.map((item) => (item.id === accepted.id ? accepted : item));
        if (accepted.state === "copying") {
          trackedCopyingOfferIds.add(accepted.id);
          announceInbox(`Copying ${offer.fileName}…`);
        } else {
          trackedCopyingOfferIds.delete(accepted.id);
          reportFinishedOffer(accepted);
        }
      }
    } catch (error) {
      announceInbox(ctx.errorMessage(error));
    } finally {
      inboxBusy = false;
      await refresh();
    }
  }

  async function refresh(): Promise<void> {
    if (inboxBusy) return;
    try {
      const next = await invoke<OfferRow[]>("list_inbox");
      // Say how a copy ended once it leaves the open list, including copies that
      // finished before the first poll saw `state === "copying"`.
      for (const before of inboxOffers) {
        if (before.state === "copying") trackedCopyingOfferIds.add(before.id);
      }
      for (const id of [...trackedCopyingOfferIds]) {
        const after = next.find((offer) => offer.id === id);
        if (!after || after.state !== "copying") {
          trackedCopyingOfferIds.delete(id);
          if (after) reportFinishedOffer(after);
        }
      }
      inboxOffers = next;
      render();
    } catch {
      // The engine may be starting or stopped; the next pass tries again.
    }
  }

  inboxStatusDismiss.addEventListener("click", () => {
    inboxStatus.textContent = "";
    inboxStatusRow.hidden = true;
    inboxStatusDismiss.hidden = true;
    render();
    ctx.queueTitle.focus({ preventScroll: true });
  });

  return { refresh };
}
