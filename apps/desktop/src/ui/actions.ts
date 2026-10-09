import type { JobSnapshot } from "../engine/types";
import { filename } from "./format";

/**
 * What the entry hosting the queue can do. The desktop turns everything on. A
 * device that only holds a profile turns off what needs this computer's
 * folders, its media tools or the browser extension, and the row says where
 * to do it instead of offering a button that cannot work.
 */
export interface QueueCapabilities {
  /** Approve or deny an agent's request. Off: the row waits, and can be cancelled. */
  approve: boolean;
  chooseNewPath: boolean;
  editLink: boolean;
  refreshSource: boolean;
  recapture: boolean;
  configureMediaTools: boolean;
  checkChecksum: boolean;
  copyPath: boolean;
}

export const fullCapabilities: QueueCapabilities = {
  approve: true,
  chooseNewPath: true,
  editLink: true,
  refreshSource: true,
  recapture: true,
  configureMediaTools: true,
  checkChecksum: true,
  copyPath: true,
};

export const ON_THIS_PC = "Do this in Fetchpath on this PC";

export interface ActionSpec {
  action: string;
  label: string;
  danger?: boolean;
}

export function actionsFor(job: JobSnapshot, capabilities: QueueCapabilities): ActionSpec[] {
  const actions: ActionSpec[] = [];
  if (job.state === "awaiting_approval") {
    if (capabilities.approve) {
      actions.push({ action: "approve", label: "Approve" });
      actions.push({ action: "deny", label: "Deny", danger: true });
    } else {
      actions.push({ action: "cancel", label: "Cancel", danger: true });
    }
  }
  if (job.state === "scheduled") actions.push({ action: "start-now", label: "Start now" });
  // Media downloads have no checkpoint to come back to, so pause is not offered
  // for them rather than offered and then refused.
  if (job.kind === "file" && ["running", "queued", "scheduled"].includes(job.state)) {
    actions.push({ action: "pause", label: "Pause" });
  }
  if (job.state === "paused") actions.push({ action: "resume", label: "Resume" });
  if (["scheduled", "queued", "running", "cancelling", "paused"].includes(job.state)) {
    actions.push({ action: "cancel", label: job.state === "cancelling" ? "Cancelling…" : "Cancel", danger: true });
  }
  if (job.state === "failed" || job.state === "cancelled" || job.state === "needs_source") {
    if (job.action === "choose_new_path") {
      if (capabilities.chooseNewPath) actions.push({ action: "choose-new-path", label: "Choose new path" });
    } else if (job.action === "edit_link") {
      if (capabilities.editLink) actions.push({ action: "edit-link", label: "Edit link" });
    } else if (job.action === "recapture") {
      if (capabilities.recapture) actions.push({ action: "recapture", label: "Send again from browser" });
    } else if (job.action === "refresh_source") {
      if (capabilities.refreshSource) actions.push({ action: "edit-link", label: "Refresh source" });
    } else if (job.action === "configure_media_tools") {
      if (capabilities.configureMediaTools) actions.push({ action: "configure-media", label: "Set up media tools" });
    } else if (job.action === "check_checksum") {
      if (capabilities.checkChecksum) {
        actions.push({ action: "edit-checksum", label: "Edit checksum" });
        actions.push({ action: "retry", label: "Retry" });
      }
    } else actions.push({ action: "retry", label: "Retry" });
  }
  if (job.state === "completed") {
    actions.push({ action: "open-folder", label: "Open folder" });
    if (capabilities.copyPath) actions.push({ action: "copy-path", label: "Copy path" });
  }
  actions.push({ action: "details", label: "Details" });
  if (["completed", "cancelled", "failed"].includes(job.state)) actions.push({ action: "remove", label: "Remove" });
  return actions;
}

/**
 * The sentence that stands in for a button the entry cannot offer, or null when
 * every action the row needs is available (always so on the desktop).
 */
export function unavailableNote(job: JobSnapshot, capabilities: QueueCapabilities): string | null {
  if (job.state === "awaiting_approval") return capabilities.approve ? null : "Approve or deny it in Fetchpath on this PC";
  if (job.state !== "failed" && job.state !== "cancelled" && job.state !== "needs_source") return null;
  const missing =
    (job.action === "choose_new_path" && !capabilities.chooseNewPath) ||
    (job.action === "edit_link" && !capabilities.editLink) ||
    (job.action === "recapture" && !capabilities.recapture) ||
    (job.action === "refresh_source" && !capabilities.refreshSource) ||
    (job.action === "configure_media_tools" && !capabilities.configureMediaTools) ||
    (job.action === "check_checksum" && !capabilities.checkChecksum);
  return missing ? ON_THIS_PC : null;
}

export function actionButton(job: JobSnapshot, spec: ActionSpec): HTMLButtonElement {
  const button = document.createElement("button");
  button.type = "button";
  // Approving is the one primary action on a request; everything else is quiet.
  button.className = spec.danger ? "secondary danger" : spec.action === "approve" ? "" : "secondary";
  button.dataset.action = spec.action;
  button.dataset.jobId = job.jobId;
  button.textContent = spec.label;
  // "Retry" repeated on five cards is five identical accessible names in the
  // automation tree, so every row action names the download it acts on.
  button.setAttribute("aria-label", `${spec.label}: ${filename(job.destination) || "download"}`);
  if (job.state === "cancelling" && spec.action === "cancel") button.disabled = true;
  return button;
}
