import type { EngineApi } from "../engine/api";
import type { JobSnapshot, JobState } from "../engine/types";
import { type QueueCapabilities, actionButton, actionsFor, unavailableNote } from "./actions";
import { required, setStatus } from "./dom";
import {
  type QueueFilter,
  approvalText,
  checksumMismatch,
  filename,
  formatBytes,
  formatDurationLong,
  friendlyError,
  isActive,
  jobLabel,
  matchesFilter,
  matchesSearch,
} from "./format";

/** What only the desktop does for a row. A missing handler leaves its action inert. */
export interface RowHandlers {
  approve?(job: JobSnapshot): Promise<void>;
  deny?(job: JobSnapshot): Promise<void>;
  chooseNewPath?(job: JobSnapshot): Promise<void>;
  editLink?(job: JobSnapshot): void;
  editChecksum?(job: JobSnapshot): void;
  configureMedia?(): Promise<void>;
}

export interface QueueContext {
  engine: EngineApi;
  capabilities: QueueCapabilities;
  handlers: RowHandlers;
  announce(message: string): void;
  announceProblem(message: string): void;
  /** Shows a failed action where the entry shows errors. */
  showError(error: unknown): void;
  /** Reloads the queue after an action changed it. */
  refresh(): Promise<void>;
  openDetails(jobId: string, opener: HTMLElement | null): void;
  /** Power mode adds a diagnostics block to each row. */
  powerMode(): boolean;
  confirmRemoveCompleted(): boolean;
  /** The count of active downloads, for the entry's header. */
  showActive(active: number): void;
  /** Runs after the summary, for whatever else the entry derives from the queue. */
  afterSummary(): void;
}

export interface QueueView {
  readonly jobs: readonly JobSnapshot[];
  /** Takes a new list from the engine and draws it. */
  update(jobs: JobSnapshot[]): void;
  render(): void;
  /** Forces the next render to rebuild every row. */
  invalidate(): void;
  setSummary(message: string): void;
  focusTitle(options?: FocusOptions): void;
  focusSearch(): void;
  detailsButton(jobId: string): HTMLElement | null;
}

/** Filters, search, summary, rows and their actions. Mount the queue markup first. */
export function createQueueView(ctx: QueueContext): QueueView {
  const { engine, capabilities, handlers } = ctx;
  const queueSearch = required<HTMLInputElement>("queue-search");
  const queueFilters = required<HTMLElement>("queue-filters");
  const queueSummary = required<HTMLParagraphElement>("queue-summary");
  const cancelCurrentButton = required<HTMLButtonElement>("cancel-download");
  const pauseAllButton = required<HTMLButtonElement>("pause-all");
  const resumeAllButton = required<HTMLButtonElement>("resume-all");
  const jobCard = required<HTMLElement>("job-card");
  const jobList = required<HTMLDivElement>("job-list");
  const queueTitle = required<HTMLHeadingElement>("queue-title");
  const queueEmpty = required<HTMLElement>("queue-empty");

  let jobs: JobSnapshot[] = [];
  let selectedFilter: QueueFilter = "all";
  let renderedQueueSignature = "";
  let observedStates = new Map<string, JobState>();

  function setQueueSummary(message: string): void {
    queueSummary.textContent = message;
    ctx.announce(message);
  }

  function renderQueue(): void {
    const focusedAction = document.activeElement instanceof HTMLButtonElement && jobList.contains(document.activeElement)
      ? { jobId: document.activeElement.dataset.jobId, action: document.activeElement.dataset.action }
      : null;
    const active = jobs.filter((job) => isActive(job.state)).length;
    ctx.showActive(active);

    const current = jobs.find((job) => ["running", "queued", "scheduled", "cancelling"].includes(job.state));
    cancelCurrentButton.hidden = !current;
    cancelCurrentButton.disabled = current?.state === "cancelling";
    cancelCurrentButton.textContent = current?.state === "cancelling" ? "Cancelling…" : "Cancel current download";
    if (current) {
      cancelCurrentButton.setAttribute(
        "aria-label",
        `Cancel the current download, ${filename(current.destination) || "download"}`,
      );
    } else {
      cancelCurrentButton.removeAttribute("aria-label");
    }

    const pausable = jobs.filter((job) => job.kind === "file" && ["running", "queued", "scheduled"].includes(job.state));
    const resumable = jobs.filter((job) => job.state === "paused");
    pauseAllButton.hidden = pausable.length < 2;
    resumeAllButton.hidden = resumable.length < 2;
    pauseAllButton.textContent = `Pause all (${pausable.length})`;
    resumeAllButton.textContent = `Resume all (${resumable.length})`;

    const query = queueSearch.value.trim().toLocaleLowerCase();
    const visible = jobs.filter((job) => matchesFilter(job, selectedFilter) && matchesSearch(job, query));
    jobCard.hidden = visible.length === 0;
    // With nothing in the list, the empty state says it all: the filters have
    // nothing to act on and the summary stays for screen readers only.
    queueEmpty.hidden = jobs.length > 0;
    queueFilters.hidden = jobs.length === 0;
    queueSummary.classList.toggle("sr-only", jobs.length === 0);

    if (!jobs.length) {
      setQueueSummary("No downloads yet. Add a link to begin.");
    } else if (!visible.length) {
      setQueueSummary("No downloads match this view.");
    } else {
      setQueueSummary(`${visible.length} of ${jobs.length} downloads shown. ${active} active.`);
    }

    // Announced after the summary so a finished or failed download is the last
    // thing queued for the polite region rather than being overwritten by it.
    announceStateChanges();
    ctx.afterSummary();

    // The queue is polled several times a second. Rebuilding identical cards would
    // churn the accessibility tree and steal keyboard focus, so it only rebuilds
    // when something structural has changed. Progress, speed and remaining time
    // change constantly and are written into the existing cards instead.
    const signature = JSON.stringify([selectedFilter, query, visible.map(queueRowSignature)]);
    if (signature === renderedQueueSignature) {
      updateLiveMetrics(visible);
      return;
    }
    renderedQueueSignature = signature;

    jobList.replaceChildren();
    visible.forEach((job, index) => jobList.append(createJobCard(job, index === 0)));
    if (!focusedAction?.jobId || !focusedAction.action) return;
    const replacement = Array.from(jobList.querySelectorAll<HTMLButtonElement>("button[data-job-id][data-action]")).find(
      (button) => button.dataset.jobId === focusedAction.jobId && button.dataset.action === focusedAction.action,
    );
    // Pause becomes Resume and Start now becomes Pause: when the exact control
    // is gone, the same row's first action is the natural place to land.
    const sameRow = replacement ?? jobList.querySelector<HTMLButtonElement>(
      `button[data-job-id="${CSS.escape(focusedAction.jobId)}"]`,
    );
    if (sameRow) {
      sameRow.focus({ preventScroll: true });
    } else {
      // The control the user was on is gone, so focus lands on a stable heading
      // instead of falling back to the document body.
      queueTitle.focus({ preventScroll: true });
    }
  }

  /**
   * Structural identity of a row.
   *
   * Deliberately excludes the byte count, rate and remaining time. Those change
   * on every poll, and including them would rebuild every card several times a
   * second, which is what `updateLiveMetrics` exists to avoid. Whether a total is
   * known *is* included, because that changes the shape of the row.
   */
  function queueRowSignature(job: JobSnapshot): string {
    return [
      job.jobId,
      job.state,
      job.totalBytes === null ? "no-total" : "total",
      job.destination ?? "",
      job.observedSha256 ?? "",
      job.expectedSha256 ?? "",
      job.error ?? "",
      job.action ?? "",
      job.notBeforeMs ?? "",
      job.qualityLabel ?? "",
      job.attempt,
      job.agent ?? "",
      job.approvalReasons.join(","),
    ].join("\u0001");
  }

  /** Writes changing measurements into cards that are already on screen. */
  function updateLiveMetrics(visible: JobSnapshot[]): void {
    for (const job of visible) {
      const card = jobList.querySelector<HTMLElement>(`[data-job-id="${CSS.escape(job.jobId)}"]`);
      if (!card) continue;
      const progress = card.querySelector("progress");
      if (progress) applyProgress(progress, job);
      const metrics = card.querySelector<HTMLElement>(".metrics");
      if (metrics) metrics.replaceChildren(...metricSpans(job));
    }
  }

  function createJobCard(job: JobSnapshot, primary: boolean): HTMLElement {
    const name = filename(job.destination) || "Download";
    const article = document.createElement("article");
    article.className = "card job";
    article.dataset.state = job.state;
    article.dataset.jobId = job.jobId;
    article.setAttribute("role", "listitem");
    article.setAttribute("aria-label", `${name}, ${jobLabel(job)}`);

    const header = document.createElement("header");
    header.className = "job-header";
    const titleWrap = document.createElement("div");
    const source = document.createElement("p");
    source.className = "job-source";
    source.textContent = job.source;
    const heading = document.createElement("h3");
    heading.textContent = name;
    titleWrap.append(heading, source);
    const status = document.createElement("output");
    status.className = "status";
    setStatus(status, job);
    if (primary) status.id = "job-status";
    header.append(titleWrap, status);
    article.append(header);

    const destination = document.createElement("p");
    destination.className = "destination";
    destination.textContent = job.qualityLabel
      ? `${job.qualityLabel} · ${job.destination ?? "Destination unavailable"}`
      : job.destination ?? "Destination unavailable";
    destination.title = destination.textContent;
    article.append(destination);

    if (job.state === "scheduled" && job.notBeforeMs) {
      const schedule = document.createElement("p");
      schedule.className = "schedule-note";
      schedule.textContent = `Scheduled for ${new Date(job.notBeforeMs).toLocaleString()}`;
      article.append(schedule);
    }

    if (job.state === "awaiting_approval") {
      const note = document.createElement("p");
      note.className = "approval-note";
      note.id = `job-approval-${job.jobId}`;
      note.textContent = approvalText(job);
      article.setAttribute("aria-describedby", note.id);
      article.append(note);
    } else if (job.agent) {
      const note = document.createElement("p");
      note.className = "schedule-note";
      note.textContent = `Requested by the agent ${job.agent}`;
      article.append(note);
    }

    // The trail: a track with a waypoint dot at its head. The native progress
    // element keeps its role, name and value; the wrapper only draws the dot.
    const trail = document.createElement("div");
    trail.className = "trail";
    const progress = document.createElement("progress");
    progress.setAttribute("aria-label", `Progress for ${name}`);
    if (primary) progress.id = "job-progress";
    trail.append(progress);
    applyProgress(progress, job);
    article.append(trail);

    const metrics = document.createElement("output");
    metrics.className = "metrics";
    // `<output>` is implicitly a polite live region. These numbers change several
    // times a second, so this one is silenced and `#live-status` announces the
    // things that matter instead.
    metrics.setAttribute("aria-live", "off");
    if (primary) metrics.id = "job-bytes";
    metrics.replaceChildren(...metricSpans(job));

    if (job.error) {
      const error = document.createElement("p");
      // A pending automatic retry is information, not a failure.
      error.className = job.state === "failed" || job.state === "needs_source" ? "error job-error" : "job-note";
      error.textContent = friendlyError(job);
      // Not a live region: `#live-alert` announces a new failure exactly once, and
      // this copy is the description of the card it belongs to.
      error.id = primary ? "job-error" : `job-error-${job.jobId}`;
      article.setAttribute("aria-describedby", error.id);
      article.append(error);
    }

    if (job.observedSha256) {
      const matched = job.state === "completed" && !!job.expectedSha256;
      const completion = document.createElement("div");
      completion.className = "completion";
      const label = document.createElement("p");
      label.className = "digest-label";
      label.textContent = matched ? "SHA-256 · matches the checksum you entered" : "Observed SHA-256";
      const digest = document.createElement("code");
      digest.textContent = job.observedSha256;
      if (primary) digest.id = "observed-hash";
      const note = document.createElement("p");
      note.className = "fine-print";
      // A match proves the bytes are the ones the checksum describes. Whether
      // the checksum itself is trustworthy depends on where it came from.
      note.textContent = matched
        ? "The file is exactly what that checksum describes. Trust it as far as you trust where the checksum came from."
        : "Observed locally; compare with a trusted publisher hash for authenticity.";
      completion.append(label, digest, note);
      article.append(completion);
    }

    const mismatch = checksumMismatch(job);
    if (mismatch) {
      const pair = document.createElement("dl");
      pair.className = "digest-pair";
      for (const [term, value] of [
        ["Expected", mismatch.expected],
        ["Received", mismatch.received],
      ]) {
        const dt = document.createElement("dt");
        dt.textContent = term;
        const dd = document.createElement("dd");
        const code = document.createElement("code");
        code.textContent = value;
        dd.append(code);
        pair.append(dt, dd);
      }
      article.append(pair);
    }

    if (ctx.powerMode()) article.append(diagnostics(job));

    // Measurements and actions share the last line, so a row stays compact.
    const footer = document.createElement("div");
    footer.className = "job-footer";
    const actions = document.createElement("div");
    actions.className = "job-actions";
    for (const action of actionsFor(job, capabilities)) actions.append(actionButton(job, action));
    const unavailable = unavailableNote(job, capabilities);
    if (unavailable) {
      const note = document.createElement("span");
      note.className = "job-note";
      note.textContent = unavailable;
      actions.append(note);
    }
    footer.append(metrics, actions);
    article.append(footer);
    return article;
  }

  /**
   * Sets the progress bar from engine-confirmed numbers only.
   *
   * With no stated total the bar stays indeterminate. Filling it from the bytes
   * received so far would show a full bar from the first chunk onward, which is
   * a lie the moment the file is larger than one read.
   */
  function applyProgress(progress: HTMLProgressElement, job: JobSnapshot): void {
    applyProgressValue(progress, job);
    // The head dot marks where the engine says the download is; an unknown
    // total or a finished file has no head to mark.
    const trail = progress.parentElement;
    if (!trail) return;
    const known = progress.hasAttribute("value") && job.state !== "completed" && progress.value > 0;
    trail.dataset.head = known ? "on" : "off";
    trail.style.setProperty("--p", `${Math.round(progress.value / progress.max * 1000) / 10}%`);
  }

  function applyProgressValue(progress: HTMLProgressElement, job: JobSnapshot): void {
    if (job.state === "completed") {
      progress.max = 1;
      progress.value = 1;
      progress.setAttribute("aria-valuetext", "Complete");
      return;
    }
    // A download that is waiting has no motion to show.
    if (job.state === "scheduled" || job.state === "queued" || job.state === "failed" || job.state === "cancelled") {
      progress.max = 1;
      progress.value = job.totalBytes ? Math.min(job.bytesReceived / job.totalBytes, 1) : 0;
      progress.setAttribute("aria-valuetext", `${formatBytes(job.bytesReceived)} received`);
      return;
    }
    if (job.totalBytes && job.totalBytes > 0) {
      const ratio = Math.min(job.bytesReceived / job.totalBytes, 1);
      progress.max = 1;
      progress.value = ratio;
      progress.setAttribute(
        "aria-valuetext",
        `${Math.floor(ratio * 100)} percent, ${formatBytes(job.bytesReceived)} of ${formatBytes(job.totalBytes)}`,
      );
      return;
    }
    progress.removeAttribute("value");
    progress.setAttribute("aria-valuetext", `${formatBytes(job.bytesReceived)} received, total size unknown`);
  }

  /** The measurements line: percent, received of total, speed, remaining. */
  function metricSpans(job: JobSnapshot): HTMLElement[] {
    const spans: HTMLElement[] = [];
    const add = (text: string, primary = false) => {
      const span = document.createElement("span");
      if (primary) span.className = "primary";
      span.textContent = text;
      spans.push(span);
    };

    // A failed or cancelled row saved nothing, so a percentage would read as success.
    const stopped = job.state === "failed" || job.state === "cancelled" || job.state === "needs_source";
    if (!stopped && job.totalBytes && job.totalBytes > 0) {
      const percent = Math.floor(Math.min(job.bytesReceived / job.totalBytes, 1) * 100);
      add(`${percent}%`, true);
      add(`${formatBytes(job.bytesReceived)} of ${formatBytes(job.totalBytes)}`);
    } else {
      add(`${formatBytes(job.bytesReceived)} received`, true);
      if (job.state === "running") add("total size unknown");
    }
    // A copy from the cache is not a transfer, so it has no speed to report.
    if (job.reusedFromCache && job.state === "completed") add("Reused from this computer's cache");
    if (job.fromPairedDevice && job.state === "completed") add(`From your paired computer ${job.fromPairedDevice}`);
    // Speed and time left describe motion; a stopped row keeps neither.
    const moving = job.state === "running";
    if (moving && job.bytesPerSecond) add(`${formatBytes(job.bytesPerSecond)}/s`);
    // Absent from the snapshot, not null, when unknown.
    if (moving && job.etaSeconds != null) add(`${formatDurationLong(job.etaSeconds)} left`);
    if (job.state === "paused") add("Paused at this point");
    return spans;
  }

  /** Power mode only. Additive detail; nothing here is needed to use the queue. */
  function diagnostics(job: JobSnapshot): HTMLElement {
    const section = document.createElement("div");
    section.className = "diagnostics";
    const label = document.createElement("p");
    label.className = "digest-label";
    label.textContent = "Diagnostics";
    const list = document.createElement("dl");
    const rows: Array<[string, string]> = [
      ["Job", job.jobId],
      ["Kind", job.kind === "torrent" ? "Torrent" : job.kind === "media" ? "Media" : "File"],
      ["Added", new Date(job.createdAtMs).toLocaleString()],
      ["Total size", job.totalBytes === null ? "Not stated by the source" : `${job.totalBytes.toLocaleString()} bytes`],
      ["Received", `${job.bytesReceived.toLocaleString()} bytes`],
    ];
    if (job.finishedAtMs) {
      rows.push(["Finished", new Date(job.finishedAtMs).toLocaleString()]);
      const elapsed = (job.finishedAtMs - job.createdAtMs) / 1000;
      if (elapsed > 0) rows.push(["Time in queue", formatDurationLong(Math.round(elapsed))]);
    }
    if (job.attempt > 0) rows.push(["Automatic retries", String(job.attempt)]);
    if (job.cleanupPending) rows.push(["Staging", "Retained for resume"]);
    for (const [term, value] of rows) {
      const dt = document.createElement("dt");
      dt.textContent = term;
      const dd = document.createElement("dd");
      dd.textContent = value;
      list.append(dt, dd);
    }
    section.append(label, list);
    return section;
  }

  /** Announces only terminal transitions, once each, through the right live region. */
  function announceStateChanges(): void {
    const next = new Map<string, JobState>();
    // An agent's request waits on the person, so it is announced when this
    // window first sees it too, not only when it changes while open.
    const asking = jobs.filter(
      (job) => job.state === "awaiting_approval" && observedStates.get(job.jobId) !== "awaiting_approval",
    );
    if (asking.length === 1) {
      ctx.announceProblem(`${approvalText(asking[0])} Approve or deny it in the queue.`);
    } else if (asking.length > 1) {
      ctx.announceProblem(`${asking.length} agent requests wait for your approval. Approve or deny them in the queue.`);
    }
    for (const job of jobs) {
      next.set(job.jobId, job.state);
      const previous = observedStates.get(job.jobId);
      if (previous === job.state || previous === undefined) continue;
      const name = filename(job.destination) || "download";
      if (job.state === "completed") {
        ctx.announce(`${name} finished downloading.`);
      } else if (job.state === "failed" || job.state === "needs_source") {
        ctx.announceProblem(`${name} needs attention. ${friendlyError(job)}`);
      } else if (job.state === "cancelled") {
        ctx.announce(`${name} was cancelled.`);
      } else if (job.state === "paused") {
        ctx.announce(`${name} is paused at ${formatBytes(job.bytesReceived)}.`);
      }
    }
    observedStates = next;
  }

  queueFilters.addEventListener("click", (event) => {
    const button = (event.target as HTMLElement).closest<HTMLButtonElement>("button[data-filter]");
    if (!button) return;
    selectedFilter = button.dataset.filter as QueueFilter;
    for (const candidate of queueFilters.querySelectorAll<HTMLButtonElement>("button[data-filter]")) {
      const selected = candidate === button;
      candidate.classList.toggle("is-selected", selected);
      candidate.setAttribute("aria-pressed", String(selected));
    }
    // The pressed state is announced by the button itself, and `renderQueue`
    // announces the resulting count, so nothing extra is pushed here.
    renderQueue();
  });

  queueSearch.addEventListener("input", renderQueue);

  cancelCurrentButton.addEventListener("click", async () => {
    const current = jobs.find((job) => ["running", "queued", "scheduled", "cancelling"].includes(job.state));
    if (!current) return;
    cancelCurrentButton.disabled = true;
    try {
      await engine.cancelDownload(current.jobId);
      await ctx.refresh();
    } catch (error) {
      ctx.showError(error);
    } finally {
      cancelCurrentButton.disabled = false;
      // The button disappears once nothing is running, so focus moves to the
      // queue heading rather than being lost with it.
      if (cancelCurrentButton.hidden) queueTitle.focus({ preventScroll: true });
      else cancelCurrentButton.focus({ preventScroll: true });
    }
  });

  pauseAllButton.addEventListener("click", () => void bulkPauseOrResume("pause"));
  resumeAllButton.addEventListener("click", () => void bulkPauseOrResume("resume"));

  /**
   * Pauses or resumes every eligible file download.
   *
   * Failures are counted rather than thrown: one row that has already finished
   * must not stop the rest, and the summary says how many actually moved.
   */
  async function bulkPauseOrResume(mode: "pause" | "resume"): Promise<void> {
    const button = mode === "pause" ? pauseAllButton : resumeAllButton;
    const targets = jobs.filter((job) =>
      mode === "pause"
        ? job.kind === "file" && ["running", "queued", "scheduled"].includes(job.state)
        : job.state === "paused",
    );
    if (!targets.length) return;
    button.disabled = true;
    let moved = 0;
    for (const job of targets) {
      try {
        await (mode === "pause" ? engine.pauseDownload(job.jobId) : engine.resumeDownload(job.jobId));
        moved += 1;
      } catch {
        // Already finished, or no longer eligible. The next refresh shows why.
      }
    }
    button.disabled = false;
    await ctx.refresh();
    const verb = mode === "pause" ? "paused" : "resumed";
    setQueueSummary(moved === 1 ? `1 download ${verb}.` : `${moved} downloads ${verb}.`);
    if (button.hidden) queueTitle.focus({ preventScroll: true });
  }

  jobList.addEventListener("click", async (event) => {
    const target = event.target as HTMLElement;
    const button = target.closest<HTMLButtonElement>("button[data-action][data-job-id]");
    if (!button) {
      // A click on the card itself opens its details, unless it was selecting
      // text (a path or a checksum) to copy.
      const card = target.closest<HTMLElement>("article.job[data-job-id]");
      if (card && !target.closest("a, code, input") && !window.getSelection()?.toString()) {
        ctx.openDetails(card.dataset.jobId!, null);
      }
      return;
    }
    if (button.dataset.action === "details") {
      ctx.openDetails(button.dataset.jobId!, button);
      return;
    }
    const jobId = button.dataset.jobId!;
    const job = jobs.find((candidate) => candidate.jobId === jobId);
    if (!job) return;
    button.disabled = true;
    try {
      switch (button.dataset.action) {
        case "cancel":
          await engine.cancelDownload(jobId);
          break;
        case "pause":
          await engine.pauseDownload(jobId);
          break;
        case "approve":
          await handlers.approve?.(job);
          break;
        case "deny":
          await handlers.deny?.(job);
          break;
        case "resume":
          await engine.resumeDownload(jobId);
          break;
        case "start-now":
          await engine.startNow(jobId);
          break;
        case "retry":
          await engine.retryDownload(jobId, null, null);
          break;
        case "choose-new-path": {
          await handlers.chooseNewPath?.(job);
          break;
        }
        case "edit-link":
          handlers.editLink?.(job);
          return;
        case "edit-checksum":
          handlers.editChecksum?.(job);
          return;
        case "recapture":
          setQueueSummary("Open the source in your browser and choose Send link to Fetchpath again.");
          return;
        case "configure-media":
          await handlers.configureMedia?.();
          return;
        case "open-folder":
          await engine.revealDownload(jobId);
          setQueueSummary("Opened the folder in File Explorer.");
          return;
        case "copy-path":
          if (job.destination) await navigator.clipboard.writeText(job.destination);
          setQueueSummary("Destination copied.");
          break;
        case "remove":
          if (
            job.state === "completed" &&
            ctx.confirmRemoveCompleted() &&
            !window.confirm(
              `Remove ${filename(job.destination) || "this download"} from the list?\n\n` +
                "The downloaded file stays on your computer.",
            )
          ) {
            return;
          }
          await engine.removeDownload(jobId);
          break;
      }
      await ctx.refresh();
    } catch (error) {
      ctx.showError(error);
    } finally {
      button.disabled = false;
      if (button.isConnected) button.focus({ preventScroll: true });
    }

  });

  return {
    get jobs() {
      return jobs;
    },
    update(next) {
      jobs = next;
      renderQueue();
    },
    render: renderQueue,
    invalidate() {
      renderedQueueSignature = "";
    },
    setSummary: setQueueSummary,
    focusTitle(options) {
      queueTitle.focus(options);
    },
    focusSearch() {
      queueSearch.focus();
      queueSearch.select();
    },
    detailsButton(jobId) {
      return jobList.querySelector<HTMLElement>(`button[data-action="details"][data-job-id="${CSS.escape(jobId)}"]`);
    },
  };
}
