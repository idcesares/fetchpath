import "./ui/mount";
import { type FolderChoice, createSocketEngine } from "./engine/socket";
import type { JobDraft } from "./engine/types";
import type { QueueCapabilities } from "./ui/actions";
import { createDetailsView } from "./ui/details";
import { required } from "./ui/dom";
import {
  formatBytes,
  joinPath,
  redactedSource,
  sanitizeFilename,
  suggestedFilename,
  uniqueDestination,
} from "./ui/format";
import { createQueueView } from "./ui/queue";

/*
 * The browser's view of the engine: the same queue and details as the desktop,
 * over /ui/socket. It views, adds, pauses, resumes, starts now, cancels,
 * removes, retries as it stands and reveals. What needs the PC's own folders,
 * media tools or browser extension is said to be done on the PC.
 */

const engine = createSocketEngine();

const noCapabilities: QueueCapabilities = {
  approve: false,
  chooseNewPath: false,
  editLink: false,
  refreshSource: false,
  recapture: false,
  configureMediaTools: false,
  checkChecksum: false,
  copyPath: false,
};

const liveStatus = required<HTMLElement>("live-status");
const liveAlert = required<HTMLElement>("live-alert");
const activeCount = required<HTMLElement>("active-count");
const activeStatText = required<HTMLElement>("active-stat-text");
const engineChip = required<HTMLElement>("engine-chip");
const engineChipText = required<HTMLElement>("engine-chip-text");
const engineStatus = required<HTMLElement>("engine-status");
const engineStatusText = required<HTMLElement>("engine-status-text");
const queueTitle = required<HTMLHeadingElement>("queue-title");

let announcedStatus = "";
let announcedAlert = "";
let dialogOpener: HTMLElement | null = null;
let problem = "";
let problemTimer = 0;

function announce(message: string): void {
  if (!message || message === announcedStatus) return;
  announcedStatus = message;
  liveStatus.textContent = message;
}

function announceProblem(message: string): void {
  if (!message) return;
  announcedAlert = message;
  liveAlert.textContent = "";
  window.setTimeout(() => {
    if (announcedAlert === message) liveAlert.textContent = message;
  }, 40);
}

function messageOf(error: unknown): string {
  return error instanceof Error ? error.message : typeof error === "string" ? error : "Something went wrong. Please try again.";
}

/** A failed row action, shown under the heading for a few seconds. */
function showProblem(error: unknown): void {
  problem = messageOf(error);
  announceProblem(problem);
  window.clearTimeout(problemTimer);
  problemTimer = window.setTimeout(() => {
    problem = "";
    renderEngine();
  }, 8000);
  renderEngine();
}

function openDialog(dialog: HTMLDialogElement, focus: HTMLElement): void {
  const active = document.activeElement;
  dialogOpener = active instanceof HTMLElement && active !== document.body ? active : null;
  dialog.showModal();
  focus.focus();
}

function restoreDialogFocus(fallback: HTMLElement): void {
  const opener = dialogOpener?.isConnected && dialogOpener.offsetParent !== null ? dialogOpener : null;
  (opener ?? fallback).focus();
  dialogOpener = null;
}

/* Queue and details --------------------------------------------------------- */

const details = createDetailsView({
  engine,
  openDialog,
  restoreFocus: restoreDialogFocus,
  rowDetailsButton: (jobId) => queue.detailsButton(jobId),
  fallbackFocus: queueTitle,
});

const queue = createQueueView({
  engine,
  capabilities: noCapabilities,
  handlers: {},
  announce,
  announceProblem,
  showError: showProblem,
  refresh: refreshQueue,
  openDetails: (jobId, opener) => void details.open(jobId, opener),
  powerMode: () => false,
  confirmRemoveCompleted: () => false,
  showActive(active) {
    activeCount.textContent = String(active);
    activeStatText.textContent =
      active === 0 ? "No downloads are active." : active === 1 ? "1 download active now." : `${active} downloads active now.`;
  },
  afterSummary: renderEngine,
});

let refreshRunning = false;
let refreshAgain = false;

async function refreshQueue(): Promise<void> {
  if (refreshRunning) {
    refreshAgain = true;
    return;
  }
  refreshRunning = true;
  try {
    const list = await engine.listDownloads();
    details.recordSpeeds(list);
    queue.update(list);
    if (details.isOpen()) await details.refresh();
  } catch (error) {
    // While the link is down the engine chip already says so.
    if (engine.linkState() === "open") showProblem(error);
  } finally {
    refreshRunning = false;
    if (refreshAgain) {
      refreshAgain = false;
      void refreshQueue();
    }
  }
}

/* The engine ---------------------------------------------------------------- */

const CHIP = {
  connecting: { state: "checking", glyph: "i-wait", text: "Connecting" },
  open: { state: "running", glyph: "i-run", text: "Engine running" },
  offline: { state: "reconnecting", glyph: "i-fail", text: "Engine not reachable" },
  refused: { state: "stopped", glyph: "i-pause", text: "Signed out" },
} as const;

function renderEngine(): void {
  const link = engine.linkState();
  const chip = CHIP[link];
  let text: string = chip.text;
  if (link === "open") {
    const rate = queue.jobs.reduce((sum, job) => sum + (job.state === "running" ? (job.bytesPerSecond ?? 0) : 0), 0);
    if (rate > 0) text = `Engine running, ${formatBytes(rate)}/s`;
  }
  if (engineChip.dataset.state !== chip.state || engineChipText.textContent !== text) {
    engineChip.dataset.state = chip.state;
    engineChipText.textContent = text;
    engineChip.querySelector("use")?.setAttribute("href", "#" + chip.glyph);
  }
  const message =
    link === "offline"
      ? "Fetchpath on this PC is not reachable. Reconnecting… Downloads continue from their saved progress once it is back."
      : link === "refused"
        ? "This page is no longer signed in. Open Fetchpath on this PC to sign in again."
        : problem;
  engineStatusText.textContent = message;
  engineStatus.hidden = message === "";
}

/* Add download -------------------------------------------------------------- */

const addDialog = required<HTMLDialogElement>("add-dialog");
const addOpenButton = required<HTMLButtonElement>("add-open");
const emptyAddButton = required<HTMLButtonElement>("empty-add");
const addCancelButton = required<HTMLButtonElement>("add-cancel");
const form = required<HTMLFormElement>("download-form");
const urlInput = required<HTMLTextAreaElement>("url");
// The shared empty state speaks of the desktop's paste and its window.
required<HTMLParagraphElement>("empty-hint").textContent = "Add a link to download it on this PC.";
required<HTMLParagraphElement>("empty-reassure").textContent = "Downloads keep running on this PC when you close this page.";

const folderSelect = required<HTMLSelectElement>("folder");
const fileNameInput = required<HTMLInputElement>("file-name");
const scheduleInput = required<HTMLInputElement>("schedule");
const batchPreview = required<HTMLElement>("batch-preview");
const previewCount = required<HTMLElement>("preview-count");
const previewList = required<HTMLOListElement>("preview-list");
const startButton = required<HTMLButtonElement>("start-download");
const formError = required<HTMLElement>("form-error");

let folderChoices: FolderChoice[] = [];

function showFormError(message: string): void {
  formError.textContent = message;
  formError.hidden = false;
  announceProblem(message);
}

function clearFormError(): void {
  formError.textContent = "";
  formError.hidden = true;
}

/** Read again on every open: the folders follow the settings on this PC. */
async function loadFolders(): Promise<void> {
  const selected = folderSelect.value;
  try {
    folderChoices = await engine.folderChoices();
  } catch (error) {
    showFormError(messageOf(error));
    return;
  }
  folderSelect.replaceChildren();
  if (folderChoices.length === 0) {
    folderSelect.append(new Option("No folder is open to the browser", ""));
    folderSelect.disabled = true;
    showFormError("Fetchpath on this PC has no folder this browser may save into.");
    return;
  }
  // The path is shown as the PC names it, so the person knows where it lands.
  for (const choice of folderChoices) folderSelect.append(new Option(`${choice.label} (${choice.path})`, choice.path));
  if (folderChoices.some((choice) => choice.path === selected)) folderSelect.value = selected;
  folderSelect.disabled = false;
}

function parseUrls(): string[] {
  return urlInput.value
    .split(/\r?\n/)
    .map((value) => value.trim())
    .filter(Boolean);
}

function buildDrafts(): JobDraft[] {
  const urls = parseUrls();
  const folder = folderSelect.value;
  if (!urls.length || !folder) return [];
  const notBeforeMs = scheduleInput.value ? new Date(scheduleInput.value).getTime() : null;
  const typedName = urls.length === 1 ? fileNameInput.value.trim() : "";
  const used = new Set<string>();
  return urls.map((url) => {
    const name = typedName ? sanitizeFilename(typedName) : suggestedFilename(url);
    const destination = uniqueDestination(joinPath(folder, name), used);
    used.add(destination.toLocaleLowerCase());
    return { url, destination, notBeforeMs, checksum: null };
  });
}

function renderPreview(): void {
  const drafts = buildDrafts();
  batchPreview.hidden = drafts.length === 0;
  previewList.replaceChildren();
  startButton.textContent = drafts.length > 1 ? `Add ${drafts.length} to queue` : "Add to queue";
  if (!drafts.length) return;
  previewCount.textContent = drafts.length === 1 ? "1 item" : `${drafts.length} items`;
  for (const draft of drafts) {
    const item = document.createElement("li");
    const name = document.createElement("strong");
    name.textContent = draft.destination.split(/[\\/]/).pop() ?? draft.destination;
    const source = document.createElement("span");
    source.textContent = redactedSource(draft.url);
    item.append(name, source);
    previewList.append(item);
  }
}

function openComposer(): void {
  clearFormError();
  openDialog(addDialog, urlInput);
  void loadFolders().then(renderPreview);
}

addOpenButton.addEventListener("click", openComposer);
emptyAddButton.addEventListener("click", openComposer);
addCancelButton.addEventListener("click", () => addDialog.close());
addDialog.addEventListener("close", () => restoreDialogFocus(addOpenButton));
for (const control of [urlInput, folderSelect, fileNameInput, scheduleInput]) {
  control.addEventListener("input", renderPreview);
  control.addEventListener("change", renderPreview);
}

form.addEventListener("submit", async (event) => {
  event.preventDefault();
  clearFormError();
  const drafts = buildDrafts();
  if (!drafts.length) {
    showFormError(parseUrls().length ? "Choose a folder to save in." : "Add at least one download address.");
    return;
  }
  startButton.disabled = true;
  startButton.textContent = "Adding…";
  try {
    await engine.startBatch(drafts);
    form.reset();
    dialogOpener = null;
    addDialog.close();
    queue.invalidate();
    queue.setSummary(drafts.length === 1 ? "Added 1 download to the queue." : `Added ${drafts.length} downloads to the queue.`);
    await refreshQueue();
  } catch (error) {
    showFormError(messageOf(error));
  } finally {
    startButton.disabled = false;
    renderPreview();
  }
});

/* Start --------------------------------------------------------------------- */

void engine.onQueueChanged(() => void refreshQueue());
void engine.onEngineChanged(() => {
  renderEngine();
  if (engine.linkState() === "open") void refreshQueue();
});
renderEngine();
engine.start();
