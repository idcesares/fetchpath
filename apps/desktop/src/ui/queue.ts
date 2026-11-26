import type { EngineApi } from "../engine/api";
import type { JobSnapshot, JobState } from "../engine/types";
import { type QueueCapabilities, actionButton, actionsFor, unavailableNote } from "./actions";
import { renderSpeedChart } from "./chart";
import { glyph, required, setStatus } from "./dom";
import {
  type QueueFilter,
  type SpeedSample,
  approvalText,
  checksumMismatch,
  fileKindMeta,
  filename,
  formatBytes,
  formatDurationLong,
  friendlyError,
  isActive,
  jobLabel,
  matchesFilter,
  matchesSearch,
  sourceHost,
  waitingForSpaceText,
} from "./format";

export type QueueViewMode = "cards" | "compact" | "grid" | "table";
export type QueueSort = "newest" | "active" | "name" | "size" | "progress";

const VIEW_KEY = "fetchpath.ui.view";
const SORT_KEY = "fetchpath.ui.sort";
const CHART_KEY = "fetchpath.ui.speedChart";
const SESSION_MAX_SAMPLES = 180;

const STATE_PRIORITY: Record<JobState, number> = {
  awaiting_approval: 0,
  running: 1,
  cancelling: 2,
  queued: 3,
  scheduled: 4,
  paused: 5,
  failed: 6,
  needs_source: 7,
  completed: 8,
  cancelled: 9,
};

function readStorage(key: string): string | null {
  try {
    return localStorage.getItem(key);
  } catch {
    return null;
  }
}

function writeStorage(key: string, value: string): void {
  try {
    localStorage.setItem(key, value);
  } catch {
    // Storage may be unavailable in restricted contexts.
  }
}

function sortJobs(list: JobSnapshot[], sort: QueueSort): JobSnapshot[] {
  if (sort === "newest") return list;
  const copy = [...list];
  copy.sort((a, b) => {
    if (sort === "active") {
      const diff = STATE_PRIORITY[a.state] - STATE_PRIORITY[b.state];
      return diff !== 0 ? diff : b.createdAtMs - a.createdAtMs;
    }
    if (sort === "name") {
      return (filename(a.destination) || a.source).localeCompare(filename(b.destination) || b.source, undefined, {
        sensitivity: "base",
        numeric: true,
      });
    }
    if (sort === "size") {
      const sizeA = a.totalBytes ?? a.bytesReceived;
      const sizeB = b.totalBytes ?? b.bytesReceived;
      return sizeB - sizeA || b.createdAtMs - a.createdAtMs;
    }
    // progress
    const progA = a.state === "completed" ? 1 : a.totalBytes ? a.bytesReceived / a.totalBytes : -1;
    const progB = b.state === "completed" ? 1 : b.totalBytes ? b.bytesReceived / b.totalBytes : -1;
    return progB - progA || b.createdAtMs - a.createdAtMs;
  });
  return copy;
}

function setIconButtonText(button: HTMLButtonElement, iconId: string, label: string): void {
  if (button.dataset.label === label) return;
  button.dataset.label = label;
  button.replaceChildren(glyph(iconId), document.createTextNode(label));
}

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
  openDialog(dialog: HTMLDialogElement, focus: HTMLElement): void;
  restoreDialogFocus(fallback: HTMLElement, dialog?: HTMLDialogElement): void;
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
  /** Forces the next render to rebuild every row. */
  invalidate(): void;
  setSummary(message: string): void;
  focusSearch(): void;
  detailsButton(jobId: string): HTMLElement | null;
  isConfirmOpen(): boolean;
  /** Runs a row action on a job (also used by the Details dialog). */
  runAction(job: JobSnapshot, action: string, trigger?: HTMLButtonElement): Promise<void>;
}

/** Filters, search, sort, display modes, live speed chart, rows and their actions. Mount markup first. */
export function createQueueView(ctx: QueueContext): QueueView {
  const { engine, capabilities, handlers } = ctx;
  const queueSearch = required<HTMLInputElement>("queue-search");
  const queueToolbar = required<HTMLElement>("queue-toolbar");
  const queueFilters = required<HTMLElement>("queue-filters");
  const queueSortSelect = required<HTMLSelectElement>("queue-sort");
  const viewModesGroup = required<HTMLElement>("view-modes");
  const toggleSpeedChart = required<HTMLButtonElement>("toggle-speed-chart");
  const sessionSpeedPanel = required<HTMLElement>("session-speed-panel");
  const sessionGraph = required<SVGSVGElement>("session-graph");
  const sessionGraphLabel = required<SVGTitleElement>("session-graph-label");
  const sessionSpeedStats = required<HTMLElement>("session-speed-stats");
  const sessionSpeedNote = required<HTMLElement>("session-speed-note");
  const queueSummary = required<HTMLParagraphElement>("queue-summary");
  const pauseAllButton = required<HTMLButtonElement>("pause-all");
  const resumeAllButton = required<HTMLButtonElement>("resume-all");
  const clearCompletedButton = required<HTMLButtonElement>("clear-completed");
  const jobCard = required<HTMLElement>("job-card");
  const tableHead = required<HTMLElement>("table-head");
  const jobList = required<HTMLDivElement>("job-list");
  const queueTitle = required<HTMLHeadingElement>("queue-title");
  const queueEmpty = required<HTMLElement>("queue-empty");
  const queueNoMatch = required<HTMLElement>("queue-no-match");
  const resetFiltersButton = required<HTMLButtonElement>("reset-filters");

  const confirmDialog = required<HTMLDialogElement>("confirm-dialog");
  const confirmTitle = required<HTMLHeadingElement>("confirm-title");
  const confirmDetail = required<HTMLParagraphElement>("confirm-detail");
  const confirmNote = required<HTMLParagraphElement>("confirm-note");
  const confirmCancel = required<HTMLButtonElement>("confirm-cancel");
  const confirmClose = required<HTMLButtonElement>("confirm-close");
  const confirmAccept = required<HTMLButtonElement>("confirm-accept");

  let jobs: JobSnapshot[] = [];
  let selectedFilter: QueueFilter = "all";
  let viewMode: QueueViewMode = (() => {
    const saved = readStorage(VIEW_KEY);
    return saved === "compact" || saved === "grid" || saved === "table" ? saved : "cards";
  })();
  let sortMode: QueueSort = (() => {
    const saved = readStorage(SORT_KEY);
    return saved === "active" || saved === "name" || saved === "size" || saved === "progress" ? saved : "newest";
  })();
  let showSpeedChart = readStorage(CHART_KEY) === "1";
  const sessionSamples: SpeedSample[] = [];
  let renderedQueueSignature = "";
  let observedStates = new Map<string, JobState>();
  let confirmResolve: ((accepted: boolean) => void) | null = null;
  let confirmFallback: HTMLElement = queueTitle;

  function applyViewMode(next: QueueViewMode): void {
    viewMode = next;
    jobCard.dataset.view = next;
    tableHead.hidden = next !== "table";
    for (const button of viewModesGroup.querySelectorAll<HTMLButtonElement>("button[data-view]")) {
      button.setAttribute("aria-pressed", String(button.dataset.view === next));
    }
    writeStorage(VIEW_KEY, next);
  }

  function applySpeedChartToggle(open: boolean): void {
    showSpeedChart = open;
    toggleSpeedChart.setAttribute("aria-pressed", String(open));
    sessionSpeedPanel.hidden = !open || jobs.length === 0;
    writeStorage(CHART_KEY, open ? "1" : "0");
    if (open && jobs.length > 0) renderSessionSpeedChart();
  }

  queueSortSelect.value = sortMode;
  applyViewMode(viewMode);
  toggleSpeedChart.setAttribute("aria-pressed", String(showSpeedChart));

  function recordSessionSpeed(list: JobSnapshot[]): void {
    const running = list.some((job) => job.state === "running");
    const combined = list.reduce((sum, job) => sum + (job.state === "running" ? (job.bytesPerSecond ?? 0) : 0), 0);
    if (!running && sessionSamples.length === 0) return;
    // Only append while running, or once right after returning to 0 so the history freezes cleanly.
    const last = sessionSamples[sessionSamples.length - 1];
    if (!running && last && last.bytesPerSecond === 0) return;
    sessionSamples.push({ at: Date.now(), bytesPerSecond: combined });
    if (sessionSamples.length > SESSION_MAX_SAMPLES) sessionSamples.shift();
  }

  function renderSessionSpeedChart(): void {
    const live = jobs.some((job) => job.state === "running");
    renderSpeedChart({
      svg: sessionGraph,
      title: sessionGraphLabel,
      statsEl: sessionSpeedStats,
      noteEl: sessionSpeedNote,
      samples: sessionSamples,
      live,
      liveWindowMs: 90_000,
      emptyMessage: live
        ? "Measuring combined download speed…"
        : "Start a download to watch live speed history, average, median, and peak throughput.",
    });
  }

  viewModesGroup.addEventListener("click", (event) => {
    const button = (event.target as HTMLElement).closest<HTMLButtonElement>("button[data-view]");
    if (!button) return;
    applyViewMode(button.dataset.view as QueueViewMode);
    renderQueue();
  });

  queueSortSelect.addEventListener("change", () => {
    sortMode = queueSortSelect.value as QueueSort;
    writeStorage(SORT_KEY, sortMode);
    renderQueue();
  });

  toggleSpeedChart.addEventListener("click", () => {
    applySpeedChartToggle(!showSpeedChart);
  });

  resetFiltersButton.addEventListener("click", () => {
    selectedFilter = "all";
    queueSearch.value = "";
    for (const candidate of queueFilters.querySelectorAll<HTMLButtonElement>("button[data-filter]")) {
      candidate.setAttribute("aria-pressed", String(candidate.dataset.filter === "all"));
    }
    renderQueue();
    queueTitle.focus({ preventScroll: true });
  });

  function askConfirm(opts: {
    title: string;
    detail: string;
    note?: string;
    acceptLabel: string;
    fallback?: HTMLElement;
  }): Promise<boolean> {
    if (confirmDialog.open) return Promise.resolve(false);
    confirmTitle.textContent = opts.title;
    confirmDetail.textContent = opts.detail;
    confirmNote.textContent = opts.note ?? "";
    confirmNote.hidden = !opts.note;
    confirmAccept.textContent = opts.acceptLabel;
    confirmFallback = opts.fallback ?? queueTitle;
    return new Promise<boolean>((resolve) => {
      confirmResolve = resolve;
      ctx.openDialog(confirmDialog, confirmCancel);
    });
  }

  function finishConfirm(accepted: boolean): void {
    const resolve = confirmResolve;
    confirmResolve = null;
    if (confirmDialog.open) confirmDialog.close();
    resolve?.(accepted);
  }

  confirmCancel.addEventListener("click", () => finishConfirm(false));
  confirmClose.addEventListener("click", () => finishConfirm(false));
  confirmAccept.addEventListener("click", () => finishConfirm(true));
  confirmDialog.addEventListener("close", () => {
    const resolve = confirmResolve;
    confirmResolve = null;
    resolve?.(false);
    ctx.restoreDialogFocus(confirmFallback, confirmDialog);
  });

  function setQueueSummary(message: string): void {
    queueSummary.textContent = message;
    ctx.announce(message);
  }

  function updateFilterCounts(): void {
    for (const button of queueFilters.querySelectorAll<HTMLButtonElement>("button[data-filter]")) {
      const filter = button.dataset.filter as QueueFilter;
      const count = jobs.filter((job) => matchesFilter(job, filter)).length;
      const badge = button.querySelector<HTMLElement>(".filter-count");
      if (!badge) continue;
      const text = count > 0 ? String(count) : "";
      if (badge.textContent !== text) badge.textContent = text;
      badge.hidden = count === 0;
    }
  }

  function renderQueue(): void {
    const focusedAction = document.activeElement instanceof HTMLButtonElement && jobList.contains(document.activeElement)
      ? { jobId: document.activeElement.dataset.jobId, action: document.activeElement.dataset.action }
      : null;
    const active = jobs.filter((job) => isActive(job.state)).length;
    ctx.showActive(active);
    updateFilterCounts();

    const pausable = jobs.filter((job) => job.kind === "file" && ["running", "queued", "scheduled"].includes(job.state));
    const resumable = jobs.filter((job) => job.state === "paused");
    const completed = jobs.filter((job) => job.state === "completed");
    pauseAllButton.hidden = pausable.length < 2;
    resumeAllButton.hidden = resumable.length < 2;
    clearCompletedButton.hidden = completed.length === 0;
    setIconButtonText(pauseAllButton, "i-pause", `Pause all (${pausable.length})`);
    setIconButtonText(resumeAllButton, "i-play", `Resume all (${resumable.length})`);
    setIconButtonText(
      clearCompletedButton,
      "i-trash",
      completed.length === 1 ? "Clear completed" : `Clear completed (${completed.length})`,
    );

    const query = queueSearch.value.trim().toLocaleLowerCase();
    const filtered = jobs.filter((job) => matchesFilter(job, selectedFilter) && matchesSearch(job, query));
    const visible = sortJobs(filtered, sortMode);
    jobCard.hidden = visible.length === 0;
    // With nothing in the list, the empty state says it all: the toolbar has
    // nothing to act on and the summary stays for screen readers only.
    queueEmpty.hidden = jobs.length > 0;
    queueToolbar.hidden = jobs.length === 0;
    queueNoMatch.hidden = jobs.length === 0 || visible.length > 0;
    sessionSpeedPanel.hidden = !showSpeedChart || jobs.length === 0;
    if (!sessionSpeedPanel.hidden) renderSessionSpeedChart();
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
    const firstCancelableId = visible.find((job) =>
      ["running", "queued", "scheduled", "cancelling"].includes(job.state),
    )?.jobId ?? "";
    const signature = JSON.stringify([
      selectedFilter,
      query,
      sortMode,
      viewMode,
      ctx.powerMode(),
      firstCancelableId,
      visible.map(queueRowSignature),
    ]);
    if (signature === renderedQueueSignature) {
      updateLiveMetrics(visible);
      return;
    }
    renderedQueueSignature = signature;

    jobList.replaceChildren();
    visible.forEach((job, index) => jobList.append(createJobCard(job, index === 0, job.jobId === firstCancelableId)));
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
      job.waitingForSpace ? "space" : "",
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
      const sizeCol = card.querySelector<HTMLElement>(".job-col-size");
      if (sizeCol) sizeCol.textContent = tableSizeText(job);
      const speedCol = card.querySelector<HTMLElement>(".job-col-speed");
      if (speedCol) speedCol.textContent = tableSpeedText(job);
    }
  }

  function tableSizeText(job: JobSnapshot): string {
    if (job.state === "completed") return formatBytes(job.totalBytes ?? job.bytesReceived);
    if (job.totalBytes && job.totalBytes > 0) {
      return `${formatBytes(job.bytesReceived)} / ${formatBytes(job.totalBytes)}`;
    }
    return formatBytes(job.bytesReceived);
  }

  function tableSpeedText(job: JobSnapshot): string {
    if (job.state === "running") {
      const parts: string[] = [];
      if (job.bytesPerSecond) parts.push(`${formatBytes(job.bytesPerSecond)}/s`);
      if (job.etaSeconds != null) parts.push(formatDurationLong(job.etaSeconds));
      return parts.join(" · ") || "Measuring…";
    }
    if (job.state === "completed" && job.reusedFromCache) return "Cached";
    if (job.state === "paused") return "Paused";
    if (job.state === "scheduled") return "Scheduled";
    return "—";
  }

  function createJobCard(job: JobSnapshot, primary: boolean, firstCancelable: boolean): HTMLElement {
    const name = filename(job.destination) || "Download";
    const meta = fileKindMeta(job);
    const article = document.createElement("article");
    article.className = "card job";
    article.dataset.state = job.state;
    article.dataset.jobId = job.jobId;
    article.setAttribute("role", "listitem");
    article.setAttribute("aria-label", `${name}, ${jobLabel(job)}`);

    const header = document.createElement("header");
    header.className = "job-header";
    const titleGroup = document.createElement("div");
    titleGroup.className = "job-title-group";
    const iconBadge = document.createElement("span");
    iconBadge.className = "kind-badge";
    iconBadge.dataset.kind = meta.category;
    iconBadge.setAttribute("aria-hidden", "true");
    iconBadge.append(glyph(meta.icon));

    const titleWrap = document.createElement("div");
    titleWrap.className = "job-title-wrap";
    const heading = document.createElement("h3");
    heading.textContent = name;
    heading.title = name;
    const source = document.createElement("p");
    source.className = "job-source";
    source.textContent = sourceHost(job.source);
    source.title = job.source;
    titleWrap.append(heading, source);
    titleGroup.append(iconBadge, titleWrap);

    const status = document.createElement("output");
    status.className = "status";
    setStatus(status, job);
    if (primary) status.id = "job-status";
    header.append(titleGroup, status);
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

    const spaceText = waitingForSpaceText(job);
    if (spaceText) {
      const space = document.createElement("p");
      space.className = "schedule-note";
      space.textContent = spaceText;
      article.append(space);
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
      completion.className = matched ? "completion completion-matched" : "completion";
      const row = document.createElement("div");
      row.className = "completion-row";
      const label = document.createElement("span");
      label.className = "digest-label";
      label.textContent = matched ? "SHA-256 · matches the checksum you entered" : "Observed SHA-256";
      const digest = document.createElement("code");
      digest.textContent = job.observedSha256;
      digest.title = matched
        ? "The file is exactly what that checksum describes. Trust it as far as you trust where the checksum came from."
        : "Observed locally; compare with a trusted publisher hash for authenticity.";
      if (primary) digest.id = "observed-hash";
      const copyHash = document.createElement("button");
      copyHash.type = "button";
      copyHash.className = "quiet copy-inline";
      copyHash.textContent = "Copy";
      copyHash.dataset.action = "copy-hash";
      copyHash.dataset.jobId = job.jobId;
      copyHash.setAttribute("aria-label", `Copy SHA-256 for ${name}`);
      row.append(label, digest, copyHash);
      const note = document.createElement("p");
      note.className = "fine-print";
      // A match proves the bytes are the ones the checksum describes. Whether
      // the checksum itself is trustworthy depends on where it came from.
      note.textContent = matched
        ? "Matches the checksum you entered; trust depends on where that checksum came from."
        : "Observed locally; compare with a trusted publisher hash for authenticity.";
      completion.append(row, note);
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

    // Table-mode columns (hidden in cards/compact/grid views via CSS).
    const sizeCol = document.createElement("span");
    sizeCol.className = "job-col-size";
    sizeCol.textContent = tableSizeText(job);
    const speedCol = document.createElement("span");
    speedCol.className = "job-col-speed";
    speedCol.textContent = tableSpeedText(job);
    article.append(sizeCol, speedCol);

    // Measurements and actions share the last line, so a row stays compact.
    const footer = document.createElement("div");
    footer.className = "job-footer";
    const actions = document.createElement("div");
    actions.className = "job-actions";
    for (const action of actionsFor(job, capabilities)) {
      const btn = actionButton(job, action);
      if (firstCancelable && action.action === "cancel") btn.id = "cancel-download";
      actions.append(btn);
    }
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
      ctx.announceProblem(`${approvalText(asking[0])} ${ctx.capabilities.approve
        ? "Approve or deny it in the queue."
        : "Approve or deny it in Fetchpath on this PC."}`);
    } else if (asking.length > 1) {
      ctx.announceProblem(`${asking.length} agent requests wait for your approval. ${ctx.capabilities.approve
        ? "Approve or deny them in the queue."
        : "Approve or deny them in Fetchpath on this PC."}`);
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
      candidate.setAttribute("aria-pressed", String(selected));
    }
    // The pressed state is announced by the button itself, and `renderQueue`
    // announces the resulting count, so nothing extra is pushed here.
    renderQueue();
  });

  queueSearch.addEventListener("input", renderQueue);

  clearCompletedButton.addEventListener("click", async () => {
    const targets = jobs.filter((job) => job.state === "completed");
    if (!targets.length) return;
    if (ctx.confirmRemoveCompleted()) {
      const ok = await askConfirm({
        title: targets.length === 1 ? "Clear 1 completed download?" : `Clear ${targets.length} completed downloads?`,
        detail: "This removes finished downloads from the list.",
        note: "Downloaded files stay on your computer.",
        acceptLabel: "Clear completed",
        fallback: clearCompletedButton,
      });
      if (!ok) return;
    }
    clearCompletedButton.disabled = true;
    let removed = 0;
    for (const job of targets) {
      try {
        await engine.removeDownload(job.jobId);
        removed += 1;
      } catch {
        // Already gone.
      }
    }
    clearCompletedButton.disabled = false;
    await ctx.refresh();
    setQueueSummary(removed === 1 ? "Cleared 1 completed download." : `Cleared ${removed} completed downloads.`);
    if (clearCompletedButton.hidden) queueTitle.focus({ preventScroll: true });
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

  async function runAction(job: JobSnapshot, action: string, button?: HTMLButtonElement): Promise<void> {
    const jobId = job.jobId;
    if (button) button.disabled = true;
    try {
      switch (action) {
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
        case "choose-new-path":
          await handlers.chooseNewPath?.(job);
          break;
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
          return;
        case "copy-hash":
          if (job.observedSha256) await navigator.clipboard.writeText(job.observedSha256);
          setQueueSummary("SHA-256 copied.");
          return;
        case "remove":
          if (job.state === "completed" && ctx.confirmRemoveCompleted()) {
            const ok = await askConfirm({
              title: "Remove from list?",
              detail: `Remove ${filename(job.destination) || "this download"} from the list?`,
              note: "The downloaded file stays on your computer.",
              acceptLabel: "Remove",
              fallback: button ?? queueTitle,
            });
            if (!ok) return;
          }
          await engine.removeDownload(jobId);
          break;
      }
      await ctx.refresh();
    } catch (error) {
      ctx.showError(error);
    } finally {
      if (button) {
        button.disabled = false;
        if (button.isConnected) button.focus({ preventScroll: true });
        else if (document.activeElement === document.body) {
          // The action can replace its own row (Pause becomes Resume). Disabling
          // the old button already released focus before renderQueue could save
          // it, so recover on the updated row or the stable queue heading.
          const next = jobList.querySelector<HTMLButtonElement>(
            `button[data-job-id="${CSS.escape(jobId)}"]:not(:disabled)`,
          );
          (next ?? queueTitle).focus({ preventScroll: true });
        }
      }
    }
  }

  jobList.addEventListener("click", async (event) => {
    const target = event.target as HTMLElement;
    const button = target.closest<HTMLButtonElement>("button[data-action][data-job-id]");
    if (!button) {
      // A click on the card itself opens its details, unless it was selecting
      // text (a path or a checksum) to copy.
      const card = target.closest<HTMLElement>("article.job[data-job-id]");
      if (card && !target.closest("a, code, input, button") && !window.getSelection()?.toString()) {
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
    await runAction(job, button.dataset.action!, button);
  });

  return {
    get jobs() {
      return jobs;
    },
    update(next) {
      jobs = next;
      recordSessionSpeed(next);
      renderQueue();
    },
    invalidate() {
      renderedQueueSignature = "";
    },
    setSummary: setQueueSummary,
    focusSearch() {
      queueSearch.focus();
      queueSearch.select();
    },
    detailsButton(jobId) {
      return jobList.querySelector<HTMLElement>(`button[data-action="details"][data-job-id="${CSS.escape(jobId)}"]`);
    },
    isConfirmOpen: () => confirmDialog.open,
    runAction,
  };
}
