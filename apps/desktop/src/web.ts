import "./ui/mount";
import { type FolderChoice, createSocketEngine } from "./engine/socket";
import type { JobDraft } from "./engine/types";
import type { QueueCapabilities } from "./ui/actions";
import { createDetailsView } from "./ui/details";
import { createDialogManager } from "./ui/dialogs";
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
const dialogs = createDialogManager();

const webCapabilities: QueueCapabilities = {
  approve: false,
  chooseNewPath: false,
  editLink: false,
  refreshSource: false,
  recapture: false,
  configureMediaTools: false,
  checkChecksum: false,
  copyPath: true,
};

const activeCount = required<HTMLElement>("active-count");
const activeStatText = required<HTMLElement>("active-stat-text");
const engineChip = required<HTMLElement>("engine-chip");
const engineChipText = required<HTMLElement>("engine-chip-text");
const engineStatus = required<HTMLElement>("engine-status");
const engineStatusText = required<HTMLElement>("engine-status-text");
const engineStatusDismiss = required<HTMLButtonElement>("engine-status-dismiss");
const pendingApprovalsButton = required<HTMLButtonElement>("pending-approvals");
const pendingApprovalsText = required<HTMLElement>("pending-approvals-text");
const queueTitle = required<HTMLHeadingElement>("queue-title");

const openPrefsButton = required<HTMLButtonElement>("open-prefs");
const prefsDialog = required<HTMLDialogElement>("prefs-dialog");
const prefsClose = required<HTMLButtonElement>("prefs-close");
const prefsCloseX = required<HTMLButtonElement>("prefs-close-x");
const webThemeSelect = required<HTMLSelectElement>("web-theme");
const webDensitySelect = required<HTMLSelectElement>("web-density");

const keyboardHelpButton = required<HTMLButtonElement>("keyboard-help");
const shortcutsDialog = required<HTMLDialogElement>("shortcuts-dialog");
const shortcutsClose = required<HTMLButtonElement>("shortcuts-close");
const shortcutsCloseX = required<HTMLButtonElement>("shortcuts-close-x");
const dropOverlay = required<HTMLElement>("drop-overlay");

let problem = "";

/* Local display preferences (theme + density, stored in this browser only) -- */

const THEME_KEY = "fetchpath.web.theme";
const DENSITY_KEY = "fetchpath.web.density";

function applyWebTheme(theme: string): void {
  if (theme === "light" || theme === "dark" || theme === "high-contrast") {
    document.documentElement.setAttribute("data-theme", theme);
  } else {
    document.documentElement.removeAttribute("data-theme");
  }
}

function applyWebDensity(density: string): void {
  if (density === "compact") document.documentElement.setAttribute("data-density", "compact");
  else document.documentElement.removeAttribute("data-density");
}

function loadWebPrefs(): void {
  try {
    const theme = localStorage.getItem(THEME_KEY) ?? "system";
    const density = localStorage.getItem(DENSITY_KEY) ?? "comfortable";
    webThemeSelect.value = theme;
    webDensitySelect.value = density;
    applyWebTheme(theme);
    applyWebDensity(density);
  } catch {
    // Storage unavailable (private browsing); keep OS defaults.
  }
}

webThemeSelect.addEventListener("change", () => {
  applyWebTheme(webThemeSelect.value);
  try {
    localStorage.setItem(THEME_KEY, webThemeSelect.value);
  } catch {}
});
webDensitySelect.addEventListener("change", () => {
  applyWebDensity(webDensitySelect.value);
  try {
    localStorage.setItem(DENSITY_KEY, webDensitySelect.value);
  } catch {}
});

openPrefsButton.addEventListener("click", () => dialogs.openDialog(prefsDialog, webThemeSelect));
prefsClose.addEventListener("click", () => prefsDialog.close());
prefsCloseX.addEventListener("click", () => prefsDialog.close());
prefsDialog.addEventListener("close", () => dialogs.restoreDialogFocus(openPrefsButton, prefsDialog));

keyboardHelpButton.addEventListener("click", () => dialogs.openDialog(shortcutsDialog, shortcutsClose));
shortcutsClose.addEventListener("click", () => shortcutsDialog.close());
shortcutsCloseX.addEventListener("click", () => shortcutsDialog.close());
shortcutsDialog.addEventListener("close", () => dialogs.restoreDialogFocus(keyboardHelpButton, shortcutsDialog));

loadWebPrefs();

function messageOf(error: unknown): string {
  return error instanceof Error ? error.message : typeof error === "string" ? error : "Something went wrong. Please try again.";
}

/** A failed row action, shown under the heading until dismissed. */
function showProblem(error: unknown): void {
  problem = messageOf(error);
  dialogs.announceProblem(problem);
  renderEngine();
}

engineStatusDismiss.addEventListener("click", () => {
  problem = "";
  renderEngine();
  queueTitle.focus({ preventScroll: true });
});

/* Queue and details --------------------------------------------------------- */

const details = createDetailsView({
  engine,
  capabilities: webCapabilities,
  openDialog: dialogs.openDialog,
  restoreFocus: dialogs.restoreDialogFocus,
  rowDetailsButton: (jobId) => queue.detailsButton(jobId),
  fallbackFocus: queueTitle,
  announce: dialogs.announce,
  onAction: (job, action, trigger) => queue.runAction(job, action, trigger),
});

const queue = createQueueView({
  engine,
  capabilities: webCapabilities,
  handlers: {},
  announce: dialogs.announce,
  announceProblem: dialogs.announceProblem,
  showError: showProblem,
  refresh: refreshQueue,
  openDetails: (jobId, opener) => void details.open(jobId, opener),
  openDialog: dialogs.openDialog,
  restoreDialogFocus: dialogs.restoreDialogFocus,
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
    if (rate > 0) text = `Engine running · ${formatBytes(rate)}/s`;
  }
  if (engineChip.dataset.state !== chip.state || engineChipText.textContent !== text) {
    engineChip.dataset.state = chip.state;
    engineChipText.textContent = text;
    engineChip.querySelector("use")?.setAttribute("href", "#" + chip.glyph);
  }
  const approvals = queue.jobs.filter((job) => job.state === "awaiting_approval").length;
  if (pendingApprovalsButton.hidden !== (approvals === 0)) pendingApprovalsButton.hidden = approvals === 0;
  const approvalsLabel = approvals === 1 ? "1 waiting for approval" : `${approvals} waiting for approval`;
  if (pendingApprovalsText.textContent !== approvalsLabel) pendingApprovalsText.textContent = approvalsLabel;

  const isConnectionNotice = link === "offline" || link === "refused";
  const message =
    link === "offline"
      ? "Fetchpath on this PC is not reachable. Reconnecting… Downloads continue from their saved progress once it is back."
      : link === "refused"
        ? "This page is no longer signed in. Open Fetchpath on this PC to sign in again."
        : problem;
  engineStatusText.textContent = message;
  engineStatus.hidden = message === "";
  engineStatusDismiss.hidden = isConnectionNotice || message === "";
}

pendingApprovalsButton.addEventListener("click", () => {
  document.querySelector<HTMLButtonElement>('button[data-filter="failed"]')?.click();
  queueTitle.focus({ preventScroll: false });
});

/* Add download -------------------------------------------------------------- */

const addDialog = required<HTMLDialogElement>("add-dialog");
const addCloseX = required<HTMLButtonElement>("add-close-x");
const addOpenButton = required<HTMLButtonElement>("add-open");
const emptyAddButton = required<HTMLButtonElement>("empty-add");
const addCancelButton = required<HTMLButtonElement>("add-cancel");
const form = required<HTMLFormElement>("download-form");
const urlInput = required<HTMLTextAreaElement>("url");
const webKindNote = required<HTMLParagraphElement>("web-kind-note");
// The shared empty state speaks of desktop torrents; adapt to direct links here.
required<HTMLParagraphElement>("empty-hint").textContent =
  "Paste a link with Ctrl+V, drag a link here, or choose Add a download to save it on this PC.";
required<HTMLParagraphElement>("empty-reassure").textContent = "Downloads keep running on this PC when you close this page.";

const folderSelect = required<HTMLSelectElement>("folder");
const fileNameField = required<HTMLElement>("file-name-field");
const fileNameInput = required<HTMLInputElement>("file-name");
const scheduleInput = required<HTMLInputElement>("schedule");
const clearScheduleButton = required<HTMLButtonElement>("clear-schedule");
const batchPreview = required<HTMLElement>("batch-preview");
const previewCount = required<HTMLElement>("preview-count");
const previewList = required<HTMLOListElement>("preview-list");
const startButton = required<HTMLButtonElement>("start-download");
const formError = required<HTMLElement>("form-error");

let folderChoices: FolderChoice[] = [];

const DESKTOP_ONLY_HOSTS = [
  "youtube.com", "youtu.be", "vimeo.com", "twitch.tv", "tiktok.com",
  "instagram.com", "soundcloud.com", "bilibili.com", "huggingface.co",
];

function desktopOnlyHint(urls: string[]): string | null {
  if (urls.some((u) => /^magnet:\?|\.torrent(?:[?#]|$)/i.test(u))) {
    return "Torrents and magnet links are added in the Fetchpath desktop app on this PC.";
  }
  if (urls.some((u) => /^hf:\/\//i.test(u))) {
    return "Hugging Face repositories are added in the Fetchpath desktop app on this PC.";
  }
  for (const raw of urls) {
    try {
      const host = new URL(raw).hostname.toLowerCase().replace(/^www\.|^m\.|^music\./, "");
      if (DESKTOP_ONLY_HOSTS.some((media) => host === media || host.endsWith(`.${media}`))) {
        return "Video pages and model repositories are inspected and added in the Fetchpath desktop app on this PC. Direct file links work here.";
      }
    } catch {
      // Invalid URL handled by validation on submit.
    }
  }
  return null;
}

function showFormError(message: string): void {
  formError.textContent = message;
  formError.hidden = false;
  dialogs.announceProblem(message);
}

function clearFormError(): void {
  formError.textContent = "";
  formError.hidden = true;
  dialogs.clearAlert();
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
  const urls = parseUrls();
  // Custom file name applies only to a single link; hide it on batches so it never silently drops input.
  fileNameField.hidden = urls.length > 1;
  const hint = desktopOnlyHint(urls);
  webKindNote.textContent = hint ?? "";
  webKindNote.hidden = !hint;

  const drafts = buildDrafts();
  batchPreview.hidden = drafts.length < 2;
  previewList.replaceChildren();
  startButton.textContent = drafts.length > 1 ? `Add ${drafts.length} to queue` : "Add to queue";
  if (drafts.length < 2) return;
  previewCount.textContent = `${drafts.length} items`;
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

function hasBlockingModalOpen(): boolean {
  return prefsDialog.open || shortcutsDialog.open || details.isOpen() || queue.isConfirmOpen();
}

function openComposer(prefill?: string): void {
  clearFormError();
  if (prefill !== undefined) {
    urlInput.value = prefill;
  }
  if (!addDialog.open) dialogs.openDialog(addDialog, urlInput);
  else urlInput.focus();
  if (prefill !== undefined) urlInput.select();
  void loadFolders().then(renderPreview);
}

addOpenButton.addEventListener("click", () => openComposer());
emptyAddButton.addEventListener("click", () => openComposer());
addCloseX.addEventListener("click", () => addDialog.close());
addCancelButton.addEventListener("click", () => {
  form.reset();
  renderPreview();
  addDialog.close();
});
addDialog.addEventListener("close", () => {
  clearFormError();
  dialogs.restoreDialogFocus(addOpenButton, addDialog);
});
clearScheduleButton.addEventListener("click", () => {
  scheduleInput.value = "";
  renderPreview();
  scheduleInput.focus();
});
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
    dialogs.clearDialogOpener(addDialog);
    addDialog.close();
    queue.invalidate();
    queue.setSummary(drafts.length === 1 ? "Added 1 download to the queue." : `Added ${drafts.length} downloads to the queue.`);
    await refreshQueue();
  } catch (error) {
    showFormError(messageOf(error));
    // Disabling the submit button drops its focus. Return to the links so
    // keyboard users can correct the rejected draft without finding it again.
    urlInput.focus();
  } finally {
    startButton.disabled = false;
    renderPreview();
  }
});

/* Keyboard shortcuts, paste-anywhere and drag-and-drop ---------------------- */

document.addEventListener("keydown", (event) => {
  if (hasBlockingModalOpen()) return;
  const key = event.key.toLowerCase();
  if (event.ctrlKey && (key === "l" || key === "n")) {
    event.preventDefault();
    openComposer();
  } else if (event.ctrlKey && key === "f" && !addDialog.open) {
    event.preventDefault();
    queue.focusSearch();
  } else if (
    event.key === "?" &&
    !event.ctrlKey &&
    !event.altKey &&
    !event.metaKey &&
    !addDialog.open &&
    !(event.target instanceof HTMLElement && event.target.closest("input, textarea, select, [contenteditable]"))
  ) {
    event.preventDefault();
    dialogs.openDialog(shortcutsDialog, shortcutsClose);
  }
});

document.addEventListener("paste", (event) => {
  const target = event.target as HTMLElement | null;
  if (target?.closest("input, textarea, select, [contenteditable]")) return;
  if (hasBlockingModalOpen()) return;
  const text = event.clipboardData?.getData("text")?.trim() ?? "";
  const links = text.split(/\s+/).filter((part) => /^https?:\/\//i.test(part));
  if (!links.length) return;
  event.preventDefault();
  openComposer(links.join("\n"));
});

let dragDepth = 0;
window.addEventListener("dragenter", (event) => {
  if (hasBlockingModalOpen()) return;
  const types = event.dataTransfer?.types ?? [];
  if (!types.includes("text/uri-list") && !types.includes("text/plain")) return;
  event.preventDefault();
  dragDepth += 1;
  dropOverlay.hidden = false;
});
window.addEventListener("dragover", (event) => {
  if (dropOverlay.hidden) return;
  event.preventDefault();
  if (event.dataTransfer) event.dataTransfer.dropEffect = "copy";
});
window.addEventListener("dragleave", () => {
  if (dragDepth > 0) dragDepth -= 1;
  if (dragDepth === 0) dropOverlay.hidden = true;
});
window.addEventListener("drop", (event) => {
  dragDepth = 0;
  dropOverlay.hidden = true;
  if (hasBlockingModalOpen()) return;
  const raw = event.dataTransfer?.getData("text/uri-list") || event.dataTransfer?.getData("text/plain") || "";
  const links = raw
    .split(/\r?\n/)
    .map((line) => line.trim())
    .filter((line) => line && !line.startsWith("#") && /^https?:\/\//i.test(line));
  if (!links.length) return;
  event.preventDefault();
  openComposer(links.join("\n"));
});

/* Start --------------------------------------------------------------------- */

void engine.onQueueChanged(() => void refreshQueue());
void engine.onEngineChanged(() => {
  renderEngine();
  if (engine.linkState() === "open") void refreshQueue();
});
renderEngine();
engine.start();
