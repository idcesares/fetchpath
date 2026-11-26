import "./ui/mount";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { save } from "@tauri-apps/plugin-dialog";
import type { EngineApi } from "./engine/api";
import { tauriEngine } from "./engine/tauri";
import type { EngineConnection, QueueStats } from "./engine/types";
import { fullCapabilities } from "./ui/actions";
import { createComposer } from "./ui/composer";
import { createDetailsView } from "./ui/details";
import { createDialogManager, errorMessage } from "./ui/dialogs";
import { required } from "./ui/dom";
import { filename, formatBytes, isActive, suggestedFilename } from "./ui/format";
import { createInboxPanel } from "./ui/inbox";
import { createQueueView, type RowHandlers } from "./ui/queue";
import { createSettingsPanel } from "./ui/settings";

const engine: EngineApi = tauriEngine;
const dialogs = createDialogManager();

const addOpenButton = required<HTMLButtonElement>("add-open");
const activeCount = required<HTMLElement>("active-count");
const activeStatText = required<HTMLElement>("active-stat-text");
const queueTitle = required<HTMLHeadingElement>("queue-title");
const keyboardHelpButton = required<HTMLButtonElement>("keyboard-help");
const shortcutsDialog = required<HTMLDialogElement>("shortcuts-dialog");
const shortcutsCloseButton = required<HTMLButtonElement>("shortcuts-close");
const shortcutsCloseX = required<HTMLButtonElement>("shortcuts-close-x");
const statsGrid = required<HTMLDListElement>("stats-grid");

const queueErrorBanner = required<HTMLElement>("queue-error-banner");
const queueError = required<HTMLElement>("queue-error");
const queueErrorDismiss = required<HTMLButtonElement>("queue-error-dismiss");
let lastQueueErrorMessage = "";

/** Links sent from the browser for review, waiting for the person to be free. */
const pendingReviews: string[] = [];
let refreshRunning = false;
/** A change arrived while a refresh was running; refresh once more after it. */
let refreshAgain = false;
let refreshTimer = 0;

function showQueueError(error: unknown): void {
  const message = errorMessage(error);
  queueError.textContent = message;
  queueErrorBanner.hidden = false;
  if (message !== lastQueueErrorMessage) {
    lastQueueErrorMessage = message;
    dialogs.announceProblem(message);
  }
}

function clearQueueError(): void {
  if (queueErrorBanner.hidden && !lastQueueErrorMessage) return;
  queueError.textContent = "";
  queueErrorBanner.hidden = true;
  lastQueueErrorMessage = "";
}

queueErrorDismiss.addEventListener("click", () => {
  clearQueueError();
  queueTitle.focus({ preventScroll: true });
});

keyboardHelpButton.addEventListener("click", () => dialogs.openDialog(shortcutsDialog, shortcutsCloseButton));
shortcutsCloseButton.addEventListener("click", () => shortcutsDialog.close());
shortcutsCloseX.addEventListener("click", () => shortcutsDialog.close());
shortcutsDialog.addEventListener("close", () => dialogs.restoreDialogFocus(keyboardHelpButton, shortcutsDialog));

/* Panels ------------------------------------------------------------------ */

const inboxPanel = createInboxPanel({
  queueTitle,
  announce: dialogs.announce,
  errorMessage,
});

const settings = createSettingsPanel({
  dialogs,
  addOpenButton,
  refreshQueue,
  showFormNotice: (message) => composer.showNotice(message),
  onMediaToolsReady: (wasReady, ready) => composer.reanalyzeIfReady(wasReady, ready),
});

const composer = createComposer({
  engine,
  dialogs,
  addOpenButton,
  defaultDestinationDir: () => settings.defaultDestinationDir,
  mediaToolsReady: () => settings.toolsStatus?.ready === true,
  hasToolsStatus: () => settings.toolsStatus !== null,
  refreshToolsStatus: () => settings.refreshToolsStatus(),
  hasBlockingModalOpen: () =>
    settings.dialog.open ||
    shortcutsDialog.open ||
    stopDialog.open ||
    engineDialog.open ||
    details.isOpen() ||
    queue.isConfirmOpen(),
  async onSubmitted(count) {
    queue.invalidate();
    queue.setSummary(count === 1 ? "Added 1 download to the queue." : `Added ${count} downloads to the queue.`);
    await refreshQueue();
  },
  onDialogClose() {
    if (pendingReviews.length) void refreshQueue();
  },
});

/* Queue and details ------------------------------------------------------- */

const rowHandlers: RowHandlers = {
  async approve(job) {
    await invoke("approve_download", { jobId: job.jobId });
    dialogs.announce(`${filename(job.destination) || "The download"} was approved.`);
  },
  async deny(job) {
    await invoke("deny_download", { jobId: job.jobId });
    dialogs.announce(`${filename(job.destination) || "The download"} was denied.`);
  },
  async chooseNewPath(job) {
    const selected = await save({
      title: "Choose a new destination",
      defaultPath: job.destination ?? suggestedFilename(job.source),
    });
    if (selected) await engine.retryDownload(job.jobId, null, selected);
  },
  editLink: (job) => composer.beginEdit(job),
  editChecksum: (job) => composer.beginChecksumEdit(job),
  configureMedia: () => settings.show("integrations"),
};

const details = createDetailsView({
  engine,
  capabilities: fullCapabilities,
  openDialog: dialogs.openDialog,
  restoreFocus: dialogs.restoreDialogFocus,
  rowDetailsButton: (jobId) => queue.detailsButton(jobId),
  fallbackFocus: queueTitle,
  announce: dialogs.announce,
  onAction: (job, action, trigger) => queue.runAction(job, action, trigger),
});

const queue = createQueueView({
  engine,
  capabilities: fullCapabilities,
  handlers: rowHandlers,
  announce: dialogs.announce,
  announceProblem: dialogs.announceProblem,
  showError: showQueueError,
  refresh: refreshQueue,
  openDetails: (jobId, opener) => void details.open(jobId, opener),
  openDialog: dialogs.openDialog,
  restoreDialogFocus: dialogs.restoreDialogFocus,
  powerMode: () => settings.view?.settings.powerMode === true,
  confirmRemoveCompleted: () => settings.view?.settings.confirmRemoveCompleted === true,
  showActive(active) {
    activeCount.textContent = String(active);
    activeStatText.textContent =
      active === 0 ? "No downloads are active." : active === 1 ? "1 download active now." : `${active} downloads active now.`;
  },
  afterSummary: renderEngineSummary,
});

document.addEventListener("keydown", (event) => {
  // While Settings, help, Engine or Stop engine is modal it owns every key. Escape in
  // Add download is the dialog's own cancel, which closes it and keeps the draft.
  if (shortcutsDialog.open || settings.dialog.open || stopDialog.open || engineDialog.open || details.isOpen() || queue.isConfirmOpen()) return;
  const key = event.key.toLowerCase();
  if (event.ctrlKey && (key === "l" || key === "n")) {
    event.preventDefault();
    composer.open();
  } else if (event.ctrlKey && event.key === ",") {
    event.preventDefault();
    void settings.show();
  } else if (event.ctrlKey && key === "f" && !composer.dialog.open) {
    event.preventDefault();
    queue.focusSearch();
  } else if (
    event.key === "?" &&
    !event.ctrlKey &&
    !event.altKey &&
    !event.metaKey &&
    !composer.dialog.open &&
    !(event.target instanceof HTMLElement && event.target.closest("input, textarea, select, [contenteditable]"))
  ) {
    event.preventDefault();
    dialogs.openDialog(shortcutsDialog, shortcutsCloseButton);
  }
});

async function refreshQueue(): Promise<void> {
  if (refreshRunning) {
    refreshAgain = true;
    return;
  }
  refreshRunning = true;
  try {
    const list = await engine.listDownloads();
    clearQueueError();
    details.recordSpeeds(list);
    queue.update(list);
    if (details.isOpen()) await details.refresh();
    if (settings.view?.settings.powerMode) await refreshStats();
    // A video page sent from the browser opens here so its quality can be
    // chosen; the link analysis picks a sensible one on its own.
    // Taking a review consumes it, so one that arrives while the person is busy
    // elsewhere waits here rather than being dropped.
    pendingReviews.push(...(await invoke<string[]>("take_link_reviews")));
    const busy = settings.dialog.open || shortcutsDialog.open || stopDialog.open || composer.hasDraft();
    if (pendingReviews.length && !busy) {
      details.close();
      composer.open(pendingReviews.splice(0).join("\n"));
      dialogs.announce("A link from your browser is ready to add.");
    }
  } catch (error) {
    if (engineState === "running") {
      showQueueError(typeof error === "string" ? error : "Could not refresh the queue.");
    }
  } finally {
    refreshRunning = false;
    if (refreshAgain) {
      refreshAgain = false;
      void refreshQueue();
    }
  }
}

/* The engine ----------------------------------------------------------------
   The queue lives in Fetchpath's engine, a background process this window is
   a client of. It tells the window when the queue changes; if it stops, the
   window says so until a new engine is running, and downloads carry on from
   their saved progress. */

const engineStatus = required<HTMLElement>("engine-status");
const engineStatusText = required<HTMLElement>("engine-status-text");
const engineStart = required<HTMLButtonElement>("engine-start");
let engineNoticeTimer = 0;
const inboxNotice = required<HTMLElement>("inbox-notice");
const inboxNoticeText = required<HTMLElement>("inbox-notice-text");
const inboxNoticeDismiss = required<HTMLButtonElement>("inbox-notice-dismiss");
/** The notice the person dismissed; a different one shows again. */
let dismissedInboxNotice = "";

const ENGINE_LOST =
  "Fetchpath's engine stopped. Reconnecting… Downloads continue from their saved progress once it is back.";
const ENGINE_STOPPED =
  "Fetchpath's engine is stopped, so downloads are waiting. Start it to continue where they left off.";

const engineChip = required<HTMLElement>("engine-chip");
const engineChipText = required<HTMLElement>("engine-chip-text");
const engineSummary = required<HTMLParagraphElement>("engine-summary");
const engineDialog = required<HTMLDialogElement>("engine-dialog");
const engineDialogSummary = required<HTMLParagraphElement>("engine-dialog-summary");
const openEngineControls = required<HTMLButtonElement>("open-engine-controls");
const engineDialogClose = required<HTMLButtonElement>("engine-dialog-close");
const closeWindowButton = required<HTMLButtonElement>("close-window");
const stopEngineButton = required<HTMLButtonElement>("stop-engine");
const stopDialog = required<HTMLDialogElement>("stop-dialog");
const stopActive = required<HTMLParagraphElement>("stop-active");
const stopHub = required<HTMLParagraphElement>("stop-hub");
const stopError = required<HTMLParagraphElement>("stop-error");
const stopCancel = required<HTMLButtonElement>("stop-cancel");
const stopCloseX = required<HTMLButtonElement>("stop-close-x");
const stopConfirm = required<HTMLButtonElement>("stop-confirm");
const pendingApprovalsButton = required<HTMLButtonElement>("pending-approvals");
const pendingApprovalsText = required<HTMLElement>("pending-approvals-text");

/** What the engine last confirmed. Nothing here is guessed: until the first
 *  report the chip says it is checking. */
let engineState: "checking" | "running" | "reconnecting" | "stopped" = "checking";

function renderEngineSummary(): void {
  const rate =
    engineState === "running"
      ? queue.jobs.reduce((sum, job) => sum + (job.state === "running" ? (job.bytesPerSecond ?? 0) : 0), 0)
      : 0;
  const chipText = {
    checking: "Checking the engine",
    running: rate > 0 ? `Engine running · ${formatBytes(rate)}/s` : "Engine running",
    reconnecting: "Engine not responding",
    stopped: "Engine stopped",
  }[engineState];
  // Called on every queue poll: an unchanged state writes nothing, so the
  // accessibility tree and keyboard focus stay put.
  if (engineChip.dataset.state !== engineState || engineChipText.textContent !== chipText) {
    engineChip.dataset.state = engineState;
    engineChipText.textContent = chipText;
    engineChip.querySelector("use")?.setAttribute(
      "href",
      "#" + { checking: "i-wait", running: "i-run", reconnecting: "i-fail", stopped: "i-pause" }[engineState],
    );
  }
  if (stopEngineButton.disabled !== (engineState === "stopped")) stopEngineButton.disabled = engineState === "stopped";

  // The counts come from the queue the engine reported; with no engine to
  // confirm them, none are claimed.
  let summary: string;
  if (engineState === "running") {
    const running = queue.jobs.filter((job) => isActive(job.state)).length;
    const waiting = queue.jobs.filter((job) => job.state === "queued" || job.state === "scheduled").length;
    const parts = [
      running ? `${running} downloading${rate > 0 ? ` at ${formatBytes(rate)}/s` : ""}` : "Nothing downloading",
      waiting ? `${waiting} waiting` : "",
    ].filter(Boolean);
    summary = `Engine running · ${parts.join(", ")}.`;
  } else if (engineState === "stopped") {
    summary = "Engine stopped. Downloads are waiting in the queue and nothing is downloading.";
  } else if (engineState === "reconnecting") {
    summary = "The engine is not responding. What is downloading is unknown until it answers.";
  } else {
    summary = "Checking the engine…";
  }

  setText(engineSummary, summary);
  setText(engineDialogSummary, summary);

  const approvals = queue.jobs.filter((job) => job.state === "awaiting_approval").length;
  if (pendingApprovalsButton.hidden !== (approvals === 0)) pendingApprovalsButton.hidden = approvals === 0;
  setText(pendingApprovalsText, approvals === 1 ? "1 waiting for approval" : `${approvals} waiting for approval`);
}

/** Writes text only when it differs, so a poll that changes nothing leaves the
 *  accessibility tree alone. */
function setText(element: HTMLElement, text: string): void {
  if (element.textContent !== text) element.textContent = text;
}

pendingApprovalsButton.addEventListener("click", () => {
  document.querySelector<HTMLButtonElement>('button[data-filter="failed"]')?.click();
  queueTitle.focus({ preventScroll: false });
});

openEngineControls.addEventListener("click", () => dialogs.openDialog(engineDialog, closeWindowButton));
engineDialogClose.addEventListener("click", () => engineDialog.close());
engineDialog.addEventListener("close", () => dialogs.restoreDialogFocus(openEngineControls, engineDialog));

closeWindowButton.addEventListener("click", () => void invoke("close_window"));

function askToStopEngine(): void {
  if (engineState === "stopped" || stopDialog.open) return;
  if (engineDialog.open) engineDialog.close();
  const running = queue.jobs.filter((job) => isActive(job.state)).length;
  stopActive.textContent =
    engineState !== "running"
      ? ""
      : running === 0
        ? "Nothing is downloading right now."
        : running === 1
          ? "1 download is running and will stop."
          : `${running} downloads are running and will stop.`;
  stopHub.hidden = settings.view?.settings.hubMode !== true;
  stopError.hidden = true;
  stopConfirm.disabled = false;
  dialogs.openDialog(stopDialog, stopCancel);
}

stopEngineButton.addEventListener("click", askToStopEngine);
stopCancel.addEventListener("click", () => stopDialog.close());
stopCloseX.addEventListener("click", () => stopDialog.close());
stopDialog.addEventListener("close", () => dialogs.restoreDialogFocus(openEngineControls, stopDialog));
stopConfirm.addEventListener("click", async () => {
  stopConfirm.disabled = true;
  try {
    await invoke("stop_engine");
    stopDialog.close();
    dialogs.announce("Fetchpath's engine is stopping. Downloads are waiting in the queue.");
    await syncEngineState();
  } catch (error) {
    stopConfirm.disabled = false;
    stopError.textContent = errorMessage(error);
    stopError.hidden = false;
  }
});

/** Reads the host's current view rather than trusting event order: two
 *  reports a moment apart may arrive in either order. */
async function syncEngineState(): Promise<void> {
  showEngineState(await engine.engineConnection());
}

function revealEngineNotice(text: string, canStart: boolean): void {
  engineStart.hidden = !canStart;
  if (!engineStatus.hidden && engineStatusText.textContent === text) return;
  engineStatusText.textContent = text;
  engineStatus.hidden = false;
  dialogs.announceProblem(text);
}

function showInboxNotice(text: string | undefined): void {
  if (!text || text === dismissedInboxNotice) {
    inboxNotice.hidden = true;
    return;
  }
  if (!inboxNotice.hidden && inboxNoticeText.textContent === text) return;
  inboxNoticeText.textContent = text;
  inboxNotice.hidden = false;
  dialogs.announceProblem(text);
}

inboxNoticeDismiss.addEventListener("click", () => {
  dismissedInboxNotice = inboxNoticeText.textContent ?? "";
  inboxNotice.hidden = true;
  queueTitle.focus({ preventScroll: true });
});

function showEngineState(state: EngineConnection): void {
  engineState = state.connected ? "running" : state.stopped ? "stopped" : "reconnecting";
  renderEngineSummary();
  showInboxNotice(state.connected ? state.inboxNotice : undefined);
  if (state.connected && !settings.view) {
    void settings.load();
    void invoke<string | null>("default_destination_dir")
      .then((dir) => {
        settings.setDefaultDestinationDir(dir);
      })
      .catch(() => {
        settings.setDefaultDestinationDir(settings.systemDownloadDir);
      });
  }
  if (state.connected && state.readOnly) {
    window.clearTimeout(engineNoticeTimer);
    engineNoticeTimer = 0;
    revealEngineNotice(state.readOnly, false);
    void refreshQueue();
    return;
  }
  if (state.connected) {
    window.clearTimeout(engineNoticeTimer);
    engineNoticeTimer = 0;
    if (!engineStatus.hidden) {
      engineStatus.hidden = true;
      dialogs.announce("Fetchpath is running again.");
      void refreshQueue();
    }
    return;
  }
  // Stopped on purpose: said at once, since nothing will change by itself.
  if (state.stopped) {
    window.clearTimeout(engineNoticeTimer);
    engineNoticeTimer = 0;
    revealEngineNotice(ENGINE_STOPPED, true);
    return;
  }
  if (!engineStatus.hidden || engineNoticeTimer) return;
  // An engine that is back within a second is not worth an alarm.
  engineNoticeTimer = window.setTimeout(async () => {
    engineNoticeTimer = 0;
    const now = await engine.engineConnection();
    if (now.connected || !engineStatus.hidden) return;
    revealEngineNotice(now.stopped ? ENGINE_STOPPED : ENGINE_LOST, Boolean(now.stopped));
  }, 1000);
}

engineStart.addEventListener("click", async () => {
  engineStart.disabled = true;
  try {
    await invoke("start_engine");
    revealEngineNotice("Starting Fetchpath's engine…", false);
  } finally {
    engineStart.disabled = false;
  }
});

async function refreshStats(): Promise<void> {
  try {
    renderStats(await engine.queueStats());
  } catch {
    // The queue itself is the source of truth on screen; a missing statistics
    // read is not worth an error banner over the whole page.
  }
}

function renderStats(stats: QueueStats): void {
  const entries: Array<[string, string]> = [
    ["Combined speed", stats.combinedBytesPerSecond ? `${formatBytes(stats.combinedBytesPerSecond)}/s` : "—"],
    ["Running", `${stats.running} of ${stats.maxActiveDownloads}`],
    ["Waiting", String(stats.queued + stats.scheduled)],
    ["Paused", String(stats.paused)],
    ["Received now", formatBytes(stats.activeBytes)],
    ["Finished", `${stats.completed} · ${formatBytes(stats.completedBytes)}`],
    ["Needs attention", String(stats.failed)],
  ];
  statsGrid.replaceChildren(
    ...entries.map(([term, value]) => {
      const group = document.createElement("div");
      const dt = document.createElement("dt");
      dt.textContent = term;
      const dd = document.createElement("dd");
      dd.textContent = value;
      group.append(dt, dd);
      return group;
    }),
  );
}

async function start(): Promise<void> {
  // Settings decide the theme, whether the statistics panel exists and whether
  // the welcome card shows, so they are read before the first paint of the
  // queue rather than after it.
  await settings.load();
  try {
    settings.setDefaultDestinationDir(await invoke<string | null>("default_destination_dir"));
  } catch {
    settings.setDefaultDestinationDir(settings.systemDownloadDir);
  }
  void settings.refreshToolsStatus();
  composer.renderPreview();
  await engine.onQueueChanged(() => void refreshQueue());
  await engine.onEngineChanged(() => void syncEngineState());
  // The tray's Stop engine item shows this window and asks here first.
  await listen("fetchpath://confirm-stop", () => askToStopEngine());
  // The tray's Open in browser failed, for instance with the web UI off.
  await listen<string>("fetchpath://web-ui-problem", async (event) => {
    await settings.showWebUiProblem(event.payload);
  });
  // The engine may have been lost before this page was listening.
  await syncEngineState();
  await refreshQueue();
  // Changes arrive as events; this slower pass catches what has no event of
  // its own, such as a schedule coming due or a link sent from the browser.
  refreshTimer = window.setInterval(() => {
    void refreshQueue();
    void inboxPanel.refresh();
  }, 1000);
  void inboxPanel.refresh();
  // Setup's finish page can start the app straight at the browser steps.
  if (await invoke<boolean>("connect_browser_requested").catch(() => false)) void settings.show("integrations");
  // A window hidden in the notification area may have its timers slowed, so
  // a link sent from the browser waits until it is shown again: refresh then.
  window.addEventListener("focus", () => void refreshQueue());
  document.addEventListener("visibilitychange", () => {
    if (document.visibilityState === "visible") void refreshQueue();
  });
  window.addEventListener("beforeunload", () => window.clearInterval(refreshTimer));
}

void start();
