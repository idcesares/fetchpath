import type { JobSnapshot, JobState } from "../engine/types";

export type QueueFilter = "all" | "active" | "paused" | "scheduled" | "completed" | "failed";

export function joinPath(directory: string | null, name: string): string {
  if (!directory) return name;
  const separator = directory.includes("/") && !directory.includes("\\") ? "/" : "\\";
  return directory.endsWith(separator) ? `${directory}${name}` : `${directory}${separator}${name}`;
}

export function siblingDestination(base: string, name: string): string {
  const separatorIndex = Math.max(base.lastIndexOf("\\"), base.lastIndexOf("/"));
  return separatorIndex < 0 ? name : `${base.slice(0, separatorIndex + 1)}${name}`;
}

export function uniqueDestination(destination: string, used: Set<string>): string {
  if (!used.has(destination.toLocaleLowerCase())) return destination;
  const dot = destination.lastIndexOf(".");
  const separator = Math.max(destination.lastIndexOf("/"), destination.lastIndexOf("\\"));
  const hasExtension = dot > separator;
  const stem = hasExtension ? destination.slice(0, dot) : destination;
  const extension = hasExtension ? destination.slice(dot) : "";
  let index = 2;
  while (used.has(`${stem} (${index})${extension}`.toLocaleLowerCase())) index += 1;
  return `${stem} (${index})${extension}`;
}

export function matchesFilter(job: JobSnapshot, filter: QueueFilter): boolean {
  if (filter === "all") return true;
  if (filter === "active") return isActive(job.state);
  if (filter === "paused") return job.state === "paused";
  if (filter === "scheduled") return job.state === "scheduled" || job.state === "queued";
  if (filter === "completed") return job.state === "completed";
  return job.state === "failed" || job.state === "needs_source" || job.state === "awaiting_approval";
}

/** Who asks and why, in the words the terminal's card uses. */
export function approvalText(job: JobSnapshot): string {
  const who = job.agent ? `The agent ${job.agent}` : "An agent";
  const why = job.approvalReasons.map((reason) => ({
    outside_granted_folders: "it would save outside the folders you let it use",
    size_limit: "it passed the size you let it download and stopped",
    rate_limit: "it asked for more downloads this hour than you allow",
    peer_discovery: "it would contact peers and discovery services",
    peer_upload: "it would upload pieces to peers",
    unknown: "it asked for something its access does not cover",
  })[reason]);
  return why.length
    ? `${who} asks to download this: ${why.join("; ")}.`
    : `${who} asks to download this.`;
}

export function matchesSearch(job: JobSnapshot, query: string): boolean {
  if (!query) return true;
  return [job.source, job.destination, filename(job.destination), job.error, job.qualityLabel, job.agent]
    .filter((value): value is string => Boolean(value))
    .some((value) => value.toLocaleLowerCase().includes(query));
}

export function isActive(state: JobState): boolean {
  return state === "running" || state === "cancelling";
}

/** The state word, or why a queued download has not started. */
export function jobLabel(job: JobSnapshot): string {
  if (job.waitingForSpace && (job.state === "queued" || job.state === "scheduled")) {
    return "Waiting for disk space";
  }
  return stateLabel(job.state);
}

export function stateLabel(state: JobState): string {
  return {
    scheduled: "Scheduled",
    queued: "Queued",
    running: "Downloading",
    paused: "Paused",
    cancelling: "Cancelling",
    completed: "Complete",
    cancelled: "Cancelled",
    failed: "Needs attention",
    needs_source: "Link needed",
    awaiting_approval: "Waiting for approval",
  }[state];
}

export function friendlyError(job: JobSnapshot): string {
  if (job.action === "choose_new_path") return "A file already exists there. Choose a different destination to continue.";
  if (job.action === "edit_link") {
    const status = job.error?.match(/HTTP status (\d{3})/)?.[1];
    if (status) {
      const reason = status === "404" || status === "410" ? "says this file isn't there" : status === "401" || status === "403" ? "refused access" : "refused this link";
      return `The website ${reason} (HTTP ${status}). Check the link, or paste a fresh one from the website.`;
    }
    return "This link needs attention. Paste a refreshed address to continue safely.";
  }
  if (job.action === "recapture") return "The protected browser context is unavailable. Send the link from your browser again.";
  if (job.action === "refresh_source") return "This media session expired or its qualities changed. Paste a refreshed source to retry.";
  if (job.action === "configure_media_tools") return "Video and audio need yt-dlp and ffmpeg. Set them up, then retry.";
  if (job.action === "check_checksum") {
    return job.error?.includes("checksum_unreadable")
      ? "The checksum saved with this download can't be read, so it won't be downloaded unchecked. Edit the checksum to continue."
      : "The downloaded file doesn't match the checksum you entered, so nothing was saved. Check the checksum, or retry if the download may have been damaged.";
  }
  return job.error ?? "The download stopped. Retry when the source is available.";
}

/** Both digests from an engine checksum-mismatch report, when there is one. */
export function checksumMismatch(job: JobSnapshot): { expected: string; received: string } | null {
  if (job.state !== "failed" || !job.error?.includes("checksum_mismatch")) return null;
  const expected = /expected sha256 ([0-9a-f]{64})/.exec(job.error)?.[1];
  const received = /(?:received|holds) ([0-9a-f]{64})/.exec(job.error)?.[1];
  return expected && received ? { expected, received } : null;
}

export function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  const units = ["KB", "MB", "GB", "TB"];
  let value = bytes / 1024;
  let unit = units[0];
  for (let index = 1; index < units.length && value >= 1024; index += 1) {
    value /= 1024;
    unit = units[index];
  }
  return `${value.toFixed(value >= 10 ? 1 : 2)} ${unit}`;
}

/** Remaining time in words, coarse on purpose: a to-the-second estimate that
 *  swings around reads as less trustworthy than an honest rough one. */
export function formatDurationLong(seconds: number): string {
  if (seconds < 60) return `${Math.max(seconds, 1)} sec`;
  if (seconds < 3600) {
    const minutes = Math.round(seconds / 60);
    return minutes === 1 ? "1 min" : `${minutes} min`;
  }
  const hours = Math.floor(seconds / 3600);
  const minutes = Math.round((seconds % 3600) / 60);
  return minutes ? `${hours} hr ${minutes} min` : `${hours} hr`;
}

export function suggestedFilename(value: string): string {
  try {
    const segments = new URL(value).pathname.split("/").filter(Boolean);
    const segment = segments[segments.length - 1];
    return sanitizeFilename(segment ? decodeURIComponent(segment) : "download.bin");
  } catch {
    return "download.bin";
  }
}

export function formatDuration(seconds: number): string {
  const rounded = Math.max(0, Math.round(seconds));
  const minutes = Math.floor(rounded / 60);
  const remainder = rounded % 60;
  return `${minutes}:${String(remainder).padStart(2, "0")}`;
}

export function sanitizeFilename(value: string): string {
  const safe = value.replace(/[<>:"/\\|?*\u0000-\u001f]/g, "_").replace(/[. ]+$/g, "");
  return safe || "download.bin";
}

export function filename(path: string | null): string {
  const segments = path?.split(/[\\/]/).filter(Boolean) ?? [];
  return segments[segments.length - 1] ?? "";
}

export function redactedSource(url: string): string {
  const index = url.search(/[?#]/);
  return index < 0 ? url : `${url.slice(0, index)}?…`;
}
