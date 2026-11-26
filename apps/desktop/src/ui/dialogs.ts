import { required } from "./dom";

/** Formats a host/engine error for display, stripping leading machine codes. */
export function errorMessage(error: unknown): string {
  const raw =
    error instanceof Error
      ? error.message
      : typeof error === "string"
        ? error
        : "Something went wrong. Please try again.";
  // Strip leading machine error codes (e.g. `helper_crash: `, `media.helper_unavailable: `).
  return raw.replace(/^[a-z][a-z0-9_]*[._][a-z0-9_.]*:\s+/i, "");
}

export interface DialogManager {
  announce(message: string): void;
  announceProblem(message: string): void;
  clearAlert(): void;
  openDialog(dialog: HTMLDialogElement, focus: HTMLElement): void;
  clearDialogOpener(dialog: HTMLDialogElement): void;
  restoreDialogFocus(fallback: HTMLElement, dialog?: HTMLDialogElement): void;
}

/**
 * Coordinates modal `<dialog>` focus restoration and ARIA live regions.
 *
 * `<dialog>` traps focus natively while modal and makes the rest of the page
 * inert, so the polite/assertive live regions always move into the topmost
 * open dialog (and back to `document.body` when the last one closes).
 */
export function createDialogManager(): DialogManager {
  const liveStatus = required<HTMLParagraphElement>("live-status");
  const liveAlert = required<HTMLParagraphElement>("live-alert");

  let announcedStatus = "";
  let announcedAlert = "";
  const dialogOpeners: Array<{ dialog: HTMLDialogElement; opener: HTMLElement | null }> = [];

  /**
   * Polite announcements. The queue polls several times a second, so the same
   * sentence is never re-sent: a screen reader would otherwise repeat it forever.
   */
  function announce(message: string): void {
    if (!message || message === announcedStatus) return;
    announcedStatus = message;
    liveStatus.textContent = message;
  }

  /** Assertive announcements, reserved for problems the user has to act on. */
  function announceProblem(message: string): void {
    if (!message) return;
    announcedAlert = message;
    liveAlert.textContent = "";
    window.setTimeout(() => {
      if (announcedAlert === message) liveAlert.textContent = message;
    }, 40);
  }

  function clearAlert(): void {
    liveAlert.textContent = "";
    announcedAlert = "";
  }

  function placeLiveRegions(): void {
    const open = document.querySelectorAll<HTMLDialogElement>("dialog[open]");
    const top = open.length ? open[open.length - 1] : null;
    const home = top ?? document.body;
    if (liveStatus.parentElement !== home) home.prepend(liveStatus, liveAlert);
  }

  for (const dialog of document.querySelectorAll("dialog")) {
    dialog.addEventListener("close", placeLiveRegions);
  }

  function resetDialogScroll(dialog: HTMLDialogElement): void {
    dialog.scrollTop = 0;
    for (const scroller of dialog.querySelectorAll<HTMLElement>(".dialog-body, .settings-form")) {
      scroller.scrollTop = 0;
    }
  }

  function openDialog(dialog: HTMLDialogElement, focus: HTMLElement): void {
    if (dialog.open) {
      placeLiveRegions();
      focus.focus({ preventScroll: true });
      return;
    }
    // Opened from the page itself (a paste, a shortcut), there is no control to
    // return to, and the dialog's fallback is used instead of the body.
    const active = document.activeElement;
    const opener = active instanceof HTMLElement && active !== document.body ? active : null;
    dialogOpeners.push({ dialog, opener });
    dialog.showModal();
    placeLiveRegions();
    resetDialogScroll(dialog);
    focus.focus({ preventScroll: true });
    // Ensure initial browser focus-scroll never pushes the dialog away from the top.
    resetDialogScroll(dialog);
  }

  function clearDialogOpener(dialog: HTMLDialogElement): void {
    for (let index = dialogOpeners.length - 1; index >= 0; index--) {
      if (dialogOpeners[index].dialog === dialog) {
        dialogOpeners[index].opener = null;
        return;
      }
    }
  }

  function restoreDialogFocus(fallback: HTMLElement, dialog?: HTMLDialogElement): void {
    let entry: { dialog: HTMLDialogElement; opener: HTMLElement | null } | undefined;
    if (dialog) {
      for (let index = dialogOpeners.length - 1; index >= 0; index--) {
        if (dialogOpeners[index].dialog === dialog) {
          entry = dialogOpeners.splice(index, 1)[0];
          break;
        }
      }
    } else {
      entry = dialogOpeners.pop();
    }
    const open = document.querySelectorAll<HTMLDialogElement>("dialog[open]");
    const top = open.length ? open[open.length - 1] : null;
    // The opener may have gone while the dialog was open, or sit behind another
    // still-open modal dialog where it is inert.
    const candidate = entry?.opener ?? null;
    const opener =
      candidate && candidate.isConnected && candidate.offsetParent !== null && (!top || top.contains(candidate))
        ? candidate
        : null;
    if (opener) {
      opener.focus();
      return;
    }
    if (top) {
      const inside = top.querySelector<HTMLElement>(
        "button:not([disabled]), input:not([disabled]), textarea:not([disabled]), select:not([disabled]), [tabindex]:not([tabindex='-1'])",
      );
      (inside ?? fallback).focus();
      return;
    }
    fallback.focus();
  }

  return {
    announce,
    announceProblem,
    clearAlert,
    openDialog,
    clearDialogOpener,
    restoreDialogFocus,
  };
}
