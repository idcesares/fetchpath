import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { open, save } from "@tauri-apps/plugin-dialog";

type JobState =
  | "scheduled"
  | "queued"
  | "running"
  | "paused"
  | "cancelling"
  | "completed"
  | "cancelled"
  | "failed"
  | "needs_source"
  | "awaiting_approval";
type QueueFilter = "all" | "active" | "paused" | "scheduled" | "completed" | "failed";

interface JobSnapshot {
  jobId: string;
  source: string;
  state: JobState;
  bytesReceived: number;
  /** Absent whenever the source never stated a length. Never guessed. */
  totalBytes: number | null;
  bytesPerSecond?: number | null;
  etaSeconds?: number | null;
  attempt: number;
  destination: string | null;
  observedSha256: string | null;
  /** The SHA-256 the person supplied. Nothing that differs from it is saved. */
  expectedSha256?: string | null;
  cleanupPending: boolean;
  error: string | null;
  action:
    | "retry"
    | "choose_new_path"
    | "edit_link"
    | "recapture"
    | "refresh_source"
    | "configure_media_tools"
    | "check_checksum"
    | null;
  retryable: boolean;
  createdAtMs: number;
  notBeforeMs: number | null;
  finishedAtMs: number | null;
  kind: "file" | "media" | "torrent";
  qualityLabel: string | null;
  /** The agent that asked for it, when an agent did. */
  agent: string | null;
  /** Why it waits for the person's approval. */
  approvalReasons: ApprovalReason[];
  /** Finished by copying verified bytes from the cache, not by a transfer. */
  reusedFromCache?: boolean;
  /** Fingerprint of the paired computer it came from. */
  fromPairedDevice?: string | null;
}

/** A model or dataset repository resolved to one commit (FP-022). */
interface RepositoryView {
  provider: string;
  kind: string;
  repo: string;
  revision: string;
  commit: string;
  files: { path: string; size?: number; sha256?: string; url: string }[];
  skipped?: string[];
  total_bytes: number;
}

/** Paired computers and sharing, as the engine reports them. */
interface PairedDevice {
  key: string;
  fingerprint: string;
  label: string;
}

interface LanView {
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

/** The content cache, as the engine reports it. */
interface CacheView {
  bytes: number;
  entries: number;
  quota_bytes: number;
  min_quota_bytes: number;
  max_quota_bytes: number;
}

type ApprovalReason = "outside_granted_folders" | "size_limit" | "rate_limit" | "peer_discovery" | "peer_upload" | "unknown";

/** One agent's access, as the engine keeps it. */
interface RuleView {
  id: number;
  label: string;
  when: string;
  then: string;
}

interface RuleAdvice {
  matched: string | null;
  folder: string | null;
  needsChecksum: boolean;
  lines: string[];
}

interface AgentView {
  name: string;
  folders: string[];
  maxBytes: number;
  maxNewJobsPerHour: number;
}

interface JobDraft {
  url: string;
  destination: string;
  notBeforeMs: number | null;
  checksum: string | null;
}

interface MediaVariant {
  id: string;
  label: string;
  kind: "video" | "audio";
  extension: string;
  height: number | null;
  fps: number | null;
}

interface MediaInspection {
  title: string;
  durationSeconds: number | null;
  variants: MediaVariant[];
}

interface Settings {
  maxActiveDownloads: number;
  defaultDestinationDir: string | null;
  autoRetry: boolean;
  autoRetryMaxAttempts: number;
  autoRetryBaseDelaySeconds: number;
  closeToTray: boolean;
  powerMode: boolean;
  mediaToolsDir: string | null;
  confirmRemoveCompleted: boolean;
  theme: "system" | "light" | "dark";
  onboardingCompleted: boolean;
  cacheQuotaBytes?: number | null;
}

interface SettingsView {
  settings: Settings;
  repaired: boolean;
  maxActiveLimit: number;
  maxRetryAttempts: number;
  systemDownloadDir: string | null;
}

interface QueueStats {
  running: number;
  queued: number;
  scheduled: number;
  paused: number;
  completed: number;
  failed: number;
  activeBytes: number;
  completedBytes: number;
  combinedBytesPerSecond: number;
  maxActiveDownloads: number;
}

interface AvailableTool {
  name: string;
  version: string;
  url: string;
  checksumSource: string;
  license: string;
  pinned: boolean;
}

interface ToolsStatus {
  ready: boolean;
  ytDlpPath: string | null;
  ffmpegDir: string | null;
  ytDlpVersion: string | null;
  ffmpegVersion: string | null;
  installDir: string;
  available: AvailableTool[];
  problem: string | null;
}

const form = required<HTMLFormElement>("download-form");
const urlInput = required<HTMLTextAreaElement>("url");
const destinationInput = required<HTMLInputElement>("destination");
const scheduleInput = required<HTMLInputElement>("schedule");
const checksumInput = required<HTMLInputElement>("checksum");
const checksumField = required<HTMLDivElement>("checksum-field");
const advancedOptions = required<HTMLDetailsElement>("advanced-options");
const clearScheduleButton = required<HTMLButtonElement>("clear-schedule");
const chooseButton = required<HTMLButtonElement>("choose-destination");
const ruleNote = required<HTMLParagraphElement>("rule-note");
const startButton = required<HTMLButtonElement>("start-download");
const formError = required<HTMLParagraphElement>("form-error");
const batchPreview = required<HTMLElement>("batch-preview");
const previewCount = required<HTMLElement>("preview-count");
const previewList = required<HTMLOListElement>("preview-list");
const queueSearch = required<HTMLInputElement>("queue-search");
const queueFilters = required<HTMLElement>("queue-filters");
const queueSummary = required<HTMLParagraphElement>("queue-summary");
const cancelCurrentButton = required<HTMLButtonElement>("cancel-download");
const pauseAllButton = required<HTMLButtonElement>("pause-all");
const resumeAllButton = required<HTMLButtonElement>("resume-all");
const jobCard = required<HTMLElement>("job-card");
const jobList = required<HTMLDivElement>("job-list");
const activeCount = required<HTMLElement>("active-count");
const kindChooser = required<HTMLFieldSetElement>("download-kind");
const mediaOptions = required<HTMLElement>("media-options");
const torrentOptions = required<HTMLElement>("torrent-options");
const torrentDiscovery = required<HTMLInputElement>("torrent-discovery");
const torrentUpload = required<HTMLInputElement>("torrent-upload");
const destinationLabel = required<HTMLLabelElement>("destination-label");
const torrentPickFile = required<HTMLButtonElement>("torrent-pick-file");
const mediaSetupNeeded = required<HTMLElement>("media-setup-needed");
const mediaInspectControls = required<HTMLElement>("media-inspect-controls");
const mediaOpenSettings = required<HTMLButtonElement>("media-open-settings");
const inspectMediaButton = required<HTMLButtonElement>("inspect-media");
const mediaQuality = required<HTMLSelectElement>("media-quality");
const mediaTitle = required<HTMLParagraphElement>("media-title");
const formNotice = required<HTMLParagraphElement>("form-notice");
const liveStatus = required<HTMLParagraphElement>("live-status");
const liveAlert = required<HTMLParagraphElement>("live-alert");
const activeStatText = required<HTMLElement>("active-stat-text");
const queueTitle = required<HTMLHeadingElement>("queue-title");
const keyboardHelpButton = required<HTMLButtonElement>("keyboard-help");
const shortcutsDialog = required<HTMLDialogElement>("shortcuts-dialog");
const shortcutsCloseButton = required<HTMLButtonElement>("shortcuts-close");
const trayNote = required<HTMLParagraphElement>("tray-note");

const onboarding = required<HTMLElement>("onboarding");
const dismissOnboarding = required<HTMLButtonElement>("dismiss-onboarding");
const onboardingOpenMedia = required<HTMLButtonElement>("onboarding-open-media");

const statsPanel = required<HTMLElement>("stats-panel");
const statsGrid = required<HTMLDListElement>("stats-grid");

const openSettingsButton = required<HTMLButtonElement>("open-settings");
const settingsDialog = required<HTMLDialogElement>("settings-dialog");
const settingsCloseButton = required<HTMLButtonElement>("settings-close");
const settingsForm = required<HTMLFormElement>("settings-form");
const settingsError = required<HTMLParagraphElement>("settings-error");
const settingsStatus = required<HTMLParagraphElement>("settings-status");
const settingsRepaired = required<HTMLParagraphElement>("settings-repaired");
const concurrencyInput = required<HTMLInputElement>("setting-concurrency");
const concurrencyValue = required<HTMLOutputElement>("setting-concurrency-value");
const settingDestination = required<HTMLInputElement>("setting-destination");
const settingChooseDestination = required<HTMLButtonElement>("setting-choose-destination");
const settingClearDestination = required<HTMLButtonElement>("setting-clear-destination");
const autoRetryInput = required<HTMLInputElement>("setting-auto-retry");
const retryAttemptsInput = required<HTMLInputElement>("setting-retry-attempts");
const retryAttemptsValue = required<HTMLOutputElement>("setting-retry-attempts-value");
const closeToTrayInput = required<HTMLInputElement>("setting-close-to-tray");
const confirmRemoveInput = required<HTMLInputElement>("setting-confirm-remove");
const powerModeInput = required<HTMLInputElement>("setting-power-mode");
const themeSelect = required<HTMLSelectElement>("setting-theme");
const mediaToolsState = required<HTMLParagraphElement>("media-tools-state");
const mediaToolsDetail = required<HTMLParagraphElement>("media-tools-detail");
const mediaToolsLicence = required<HTMLParagraphElement>("media-tools-licence");
const mediaToolsInstall = required<HTMLButtonElement>("media-tools-install");
const mediaToolsLocate = required<HTMLButtonElement>("media-tools-locate");

const addDialog = required<HTMLDialogElement>("add-dialog");
const addOpenButton = required<HTMLButtonElement>("add-open");
const addCancelButton = required<HTMLButtonElement>("add-cancel");
const emptyAddButton = required<HTMLButtonElement>("empty-add");
const queueEmpty = required<HTMLElement>("queue-empty");

const browserSetupState = required<HTMLParagraphElement>("browser-setup-state");
const browserSetupDetail = required<HTMLParagraphElement>("browser-setup-detail");
const browserSetupSteps = required<HTMLOListElement>("browser-setup-steps");
const browserOpenFolder = required<HTMLButtonElement>("browser-open-folder");
const browserCopyChrome = required<HTMLButtonElement>("browser-copy-chrome");
const browserCopyEdge = required<HTMLButtonElement>("browser-copy-edge");
const browserCopyFolder = required<HTMLButtonElement>("browser-copy-folder");
const browserExtensionPath = required<HTMLElement>("browser-extension-path");

interface BrowserSetupStatus {
  extensionDir: string | null;
  browsers: Array<{ browser: string; registered: boolean }>;
}

const detailsDialog = required<HTMLDialogElement>("details-dialog");
const detailsTitle = required<HTMLElement>("details-title");
const detailsSource = required<HTMLElement>("details-source");
const detailsStatus = required<HTMLOutputElement>("details-status");
const detailsFigures = required<HTMLElement>("details-figures");
const detailsGraph = required<SVGSVGElement>("details-graph");
const detailsGraphLabel = required<SVGTitleElement>("details-graph-label");
const detailsGraphNote = required<HTMLElement>("details-graph-note");
const detailsMap = required<SVGSVGElement>("details-map");
const detailsMapLabel = required<SVGTitleElement>("details-map-label");
const detailsSegments = required<HTMLOListElement>("details-segments");
const detailsSegmentsNote = required<HTMLElement>("details-segments-note");
const detailsInfo = required<HTMLElement>("details-info");
const detailsClose = required<HTMLButtonElement>("details-close");

/** One byte range in flight: received into memory, not yet written. */
interface SegmentView {
  start: number;
  /** Inclusive. */
  end: number;
  received: number;
}

interface JobDetails {
  job: JobSnapshot;
  segments: SegmentView[];
}

/** How far back the speed graph looks. */
const SPEED_WINDOW_MS = 60_000;

let jobs: JobSnapshot[] = [];
/**
 * Speed samples per running download, taken from the engine-measured rate on
 * every queue refresh, so the graph already has history when it is opened.
 * Live only; a restart starts a new history.
 */
const speedHistory = new Map<string, Array<{ at: number; bytesPerSecond: number }>>();
let detailsJobId: string | null = null;
/** Links sent from the browser for review, waiting for the person to be free. */
const pendingReviews: string[] = [];
let selectedFilter: QueueFilter = "all";
let editingJobId: string | null = null;
/** True while the composer is correcting a checksum, where the link is optional. */
let editingChecksum = false;
let refreshRunning = false;
/** A change arrived while a refresh was running; refresh once more after it. */
let refreshAgain = false;
let refreshTimer = 0;
let mediaInspection: MediaInspection | null = null;
let inspectedMediaUrl = "";
let renderedQueueSignature = "";
let announcedStatus = "";
let announcedAlert = "";
let dialogOpener: HTMLElement | null = null;
let composerFocus: HTMLElement | null = null;
let observedStates = new Map<string, JobState>();
/// True while the destination still holds Fetchpath's own suggestion rather
/// than something the user typed or picked.
let destinationIsSuggested = true;
/// The folder the person's rules choose for the first link, when one does.
/// It replaces the default save folder in Fetchpath's own suggestion only.
let ruleFolder: string | null = null;
let advisedUrl = "";

let settingsView: SettingsView | null = null;
let toolsStatus: ToolsStatus | null = null;
let systemDownloadDir: string | null = null;
let defaultDestinationDir: string | null = null;

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

function setQueueSummary(message: string): void {
  queueSummary.textContent = message;
  announce(message);
}

/* Dialogs ------------------------------------------------------------------
   `<dialog>` traps focus natively while modal. Every open records its trigger
   and every close restores it, including the Escape path, so keyboard focus is
   never dropped onto the document body. */

function openDialog(dialog: HTMLDialogElement, focus: HTMLElement): void {
  // Opened from the page itself (a paste, a shortcut), there is no control to
  // return to, and the dialog's fallback is used instead of the body.
  const active = document.activeElement;
  dialogOpener = active instanceof HTMLElement && active !== document.body ? active : null;
  dialog.showModal();
  placeLiveRegions();
  focus.focus();
}

/**
 * A modal dialog makes the rest of the page inert, and inert live regions are
 * never announced. The two regions therefore live in whichever dialog is on
 * top, and go back to the page when the last one closes.
 */
function placeLiveRegions(): void {
  const open = document.querySelectorAll<HTMLDialogElement>("dialog[open]");
  const top = open.length ? open[open.length - 1] : null;
  const home = top ?? document.body;
  if (liveStatus.parentElement !== home) home.prepend(liveStatus, liveAlert);
}

for (const dialog of document.querySelectorAll("dialog")) dialog.addEventListener("close", placeLiveRegions);

function restoreDialogFocus(fallback: HTMLElement): void {
  // The opener may have gone while the dialog was open: the empty state's Add
  // button disappears once the first download exists.
  const opener = dialogOpener?.isConnected && dialogOpener.offsetParent !== null ? dialogOpener : null;
  (opener ?? fallback).focus();
  dialogOpener = null;
}

keyboardHelpButton.addEventListener("click", () => openDialog(shortcutsDialog, shortcutsCloseButton));
shortcutsCloseButton.addEventListener("click", () => shortcutsDialog.close());
shortcutsDialog.addEventListener("close", () => restoreDialogFocus(keyboardHelpButton));

openSettingsButton.addEventListener("click", () => void showSettings());
settingsCloseButton.addEventListener("click", () => settingsDialog.close());
settingsDialog.addEventListener("close", () => restoreDialogFocus(openSettingsButton));

// Every setting saves the moment it changes, so the settings form has nothing
// to submit. Without this, Enter in the folder field triggers implicit
// submission, which navigates the webview away from the application.
settingsForm.addEventListener("submit", (event) => event.preventDefault());

const settingsCategories = [...settingsDialog.querySelectorAll<HTMLButtonElement>("[data-settings-category]")];
for (const button of settingsCategories) {
  button.addEventListener("click", () => {
    const category = button.dataset.settingsCategory;
    for (const choice of settingsCategories) choice.setAttribute("aria-pressed", String(choice === button));
    for (const panel of settingsForm.querySelectorAll<HTMLElement>("[data-settings-panel]")) {
      panel.hidden = panel.dataset.settingsPanel !== category;
    }
    settingsForm.setAttribute("aria-label", `${button.textContent} settings`);
    settingsForm.scrollTop = 0;
  });
}
required<HTMLAnchorElement>("project-attribution").addEventListener("click", async (event) => {
  event.preventDefault();
  clearError(settingsError);
  try {
    await invoke("open_project_page");
  } catch (error) {
    showError(settingsError, error);
  }
});

async function showSettings(category?: string): Promise<void> {
  if (category) settingsCategories.find((button) => button.dataset.settingsCategory === category)?.click();
  clearError(settingsError);
  openDialog(settingsDialog, settingsCategories.find((button) => button.getAttribute("aria-pressed") === "true") ?? settingsCloseButton);
  await Promise.all([loadSettings(), refreshToolsStatus(), refreshBrowserSetup(), refreshCliStatus(), refreshAgents(), refreshRules(), refreshCache(), refreshLan()]);
}

/* Rules (FP-075) -------------------------------------------------------------
   The engine keeps and applies the rules for every client; Settings lists,
   adds, removes and tests them in the words the command line uses. */

const rulesList = required<HTMLOListElement>("rules-list");
const ruleName = required<HTMLInputElement>("rule-name");
const ruleDomains = required<HTMLInputElement>("rule-domains");
const ruleTypes = required<HTMLInputElement>("rule-types");
const ruleMinSize = required<HTMLInputElement>("rule-min-size");
const ruleMaxSize = required<HTMLInputElement>("rule-max-size");
const ruleFolderInput = required<HTMLInputElement>("rule-folder");
const ruleChooseFolder = required<HTMLButtonElement>("rule-choose-folder");
const ruleQuality = required<HTMLSelectElement>("rule-quality");
const ruleConnections = required<HTMLSelectElement>("rule-connections");
const ruleChecksum = required<HTMLInputElement>("rule-checksum");
const ruleSave = required<HTMLButtonElement>("rule-save");
const ruleTestLink = required<HTMLInputElement>("rule-test-link");
const ruleTest = required<HTMLButtonElement>("rule-test");
const ruleTestResult = required<HTMLUListElement>("rule-test-result");
let rules: RuleView[] = [];
const cacheUsage = required<HTMLParagraphElement>("cache-usage");
const cacheQuotaInput = required<HTMLInputElement>("setting-cache-quota");
const cacheQuotaHint = required<HTMLParagraphElement>("setting-cache-quota-hint");
const cacheClear = required<HTMLButtonElement>("cache-clear");
const GB = 1024 ** 3;

function renderCache(cache: CacheView): void {
  cacheUsage.textContent = cache.entries
    ? `${formatBytes(cache.bytes)} in ${cache.entries} ${cache.entries === 1 ? "file" : "files"}, of ${formatBytes(cache.quota_bytes)}.`
    : `Empty. It keeps up to ${formatBytes(cache.quota_bytes)}.`;
  cacheQuotaInput.min = String(cache.min_quota_bytes / GB);
  cacheQuotaInput.max = String(cache.max_quota_bytes / GB);
  cacheQuotaInput.value = String(Math.round((cache.quota_bytes / GB) * 100) / 100);
  cacheQuotaHint.textContent = `Between ${formatBytes(cache.min_quota_bytes)} and ${formatBytes(cache.max_quota_bytes)}. The oldest files go first when it is full.`;
  cacheClear.disabled = cache.entries === 0;
}

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

function renderLan(lan: LanView): void {
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
      clearError(settingsError);
      try {
        renderLan(await invoke<LanView>("unpair_device", { key: device.key }));
        agentStatus(`${device.label} is no longer paired. It can't get files from this computer.`);
        lanPair.focus();
      } catch (error) {
        showError(settingsError, error);
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
    if (pairing.state !== lanPairingState && pairing.state !== "waiting") announce(lanPairingText.textContent);
  }
  lanPairingState = pairing?.state;
  const waiting = pairing?.state === "waiting";
  if (waiting && lanPoll === undefined) {
    lanPoll = window.setInterval(() => void refreshLan(), 1000);
  } else if (!waiting && lanPoll !== undefined) {
    window.clearInterval(lanPoll);
    lanPoll = undefined;
  }
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

async function refreshLan(): Promise<void> {
  try {
    renderLan(await invoke<LanView>("lan_status"));
  } catch (error) {
    lanSelf.textContent = "Paired computers can't be shown right now.";
    if (lanPoll !== undefined) {
      window.clearInterval(lanPoll);
      lanPoll = undefined;
    }
    showError(settingsError, error);
  }
}

lanSharing.addEventListener("change", async () => {
  clearError(settingsError);
  try {
    renderLan(await invoke<LanView>("set_lan_sharing", { enabled: lanSharing.checked }));
    announce(lanSharingState.textContent ?? "");
  } catch (error) {
    showError(settingsError, error);
    await refreshLan();
  }
});

lanPair.addEventListener("click", async () => {
  clearError(settingsError);
  try {
    renderLan(await invoke<LanView>("start_pairing"));
    announce(`Pairing code ${lanCode.textContent}. ${lanPairingText.textContent}`);
  } catch (error) {
    showError(settingsError, error);
  }
});

lanPairCancel.addEventListener("click", async () => {
  clearError(settingsError);
  try {
    renderLan(await invoke<LanView>("cancel_pairing"));
    agentStatus("The pairing code was withdrawn.");
    lanPair.focus();
  } catch (error) {
    showError(settingsError, error);
  }
});

lanJoin.addEventListener("click", async () => {
  clearError(settingsError);
  lanJoinResult.textContent = "Pairing…";
  lanJoin.disabled = true;
  try {
    const device = await invoke<PairedDevice>("join_pairing", {
      address: lanJoinAddress.value,
      code: lanJoinCode.value,
      label: lanJoinName.value || null,
    });
    lanJoinCode.value = "";
    await refreshLan();
    lanJoinResult.textContent = `Paired with ${device.label}, fingerprint ${device.fingerprint}. Check that the other computer shows the same.`;
  } catch (error) {
    lanJoinResult.textContent = String(error);
  } finally {
    lanJoin.disabled = false;
  }
});

async function refreshCache(): Promise<void> {
  try {
    renderCache(await invoke<CacheView>("cache_status"));
  } catch (error) {
    cacheUsage.textContent = "The cache cannot be read right now.";
    showError(settingsError, error);
  }
}

cacheQuotaInput.addEventListener("change", async () => {
  const gigabytes = Number(cacheQuotaInput.value);
  if (!Number.isFinite(gigabytes) || gigabytes <= 0) {
    await refreshCache();
    return;
  }
  // The engine clamps the quota to its bounds; the form then shows what it kept.
  await changeSetting({ cacheQuotaBytes: Math.round(gigabytes * GB) }, "Cache size updated.");
  await refreshCache();
});

cacheClear.addEventListener("click", async () => {
  clearError(settingsError);
  try {
    renderCache(await invoke<CacheView>("clear_cache"));
    settingsStatus.textContent = "Cache cleared. Your saved downloads are not affected.";
    settingsStatus.hidden = false;
    announce(settingsStatus.textContent);
  } catch (error) {
    showError(settingsError, error);
  }
});

async function refreshRules(): Promise<void> {
  try {
    rules = await invoke<RuleView[]>("list_rules");
    renderRules();
  } catch (error) {
    showError(settingsError, error);
  }
}

function renderRules(): void {
  rulesList.replaceChildren();
  if (!rules.length) {
    const empty = document.createElement("li");
    empty.className = "field-note";
    empty.textContent = "No rules yet. Every download goes where you choose, or to the default save folder.";
    rulesList.append(empty);
    return;
  }
  for (const rule of rules) {
    const item = document.createElement("li");
    const row = document.createElement("div");
    row.className = "rule-row";
    const text = document.createElement("div");
    const heading = document.createElement("h3");
    heading.id = `rule-${rule.id}`;
    heading.textContent = rule.label;
    const summary = document.createElement("p");
    summary.className = "hint field-note";
    summary.textContent = `${rule.when} → ${rule.then}`;
    text.append(heading, summary);
    const remove = document.createElement("button");
    remove.type = "button";
    remove.className = "secondary danger";
    remove.textContent = "Remove";
    remove.dataset.ruleId = String(rule.id);
    remove.setAttribute("aria-label", `Remove ${rule.label}`);
    row.append(text, remove);
    item.append(row);
    rulesList.append(item);
  }
}

function listOf(value: string): string[] {
  return value.split(",").map((item) => item.trim().replace(/^\./, "")).filter(Boolean);
}

function megabytes(input: HTMLInputElement): number | undefined {
  const value = input.value.trim();
  if (!value) return undefined;
  return Math.round(Number(value) * MIB);
}

rulesList.addEventListener("click", async (event) => {
  const button = (event.target as HTMLElement).closest<HTMLButtonElement>("button[data-rule-id]");
  if (!button) return;
  const label = rules.find((rule) => String(rule.id) === button.dataset.ruleId)?.label ?? "The rule";
  clearError(settingsError);
  try {
    rules = await invoke<RuleView[]>("remove_rule", { ruleId: Number(button.dataset.ruleId) });
    renderRules();
    agentStatus(`${label} removed.`);
  } catch (error) {
    showError(settingsError, error);
  }
  (rulesList.querySelector<HTMLElement>("button[data-rule-id]") ?? ruleTestLink).focus();
});

ruleChooseFolder.addEventListener("click", async () => {
  const selected = await open({ directory: true, title: "Choose a folder for this rule" }).catch(() => null);
  if (typeof selected === "string") ruleFolderInput.value = selected;
  ruleFolderInput.focus();
});

ruleSave.addEventListener("click", async () => {
  clearError(settingsError);
  const connections = ruleConnections.value ? Number(ruleConnections.value) : undefined;
  const rule = {
    name: ruleName.value.trim() || undefined,
    when: {
      domains: listOf(ruleDomains.value.toLowerCase()),
      file_types: listOf(ruleTypes.value.toLowerCase()),
      min_size_bytes: megabytes(ruleMinSize),
      max_size_bytes: megabytes(ruleMaxSize),
    },
    then: {
      folder: ruleFolderInput.value.trim() || undefined,
      media_quality: ruleQuality.value || undefined,
      require_checksum: ruleChecksum.checked,
      max_connections: connections,
    },
  };
  try {
    rules = await invoke<RuleView[]>("add_rule", { rule });
    renderRules();
    for (const input of [ruleName, ruleDomains, ruleTypes, ruleMinSize, ruleMaxSize, ruleFolderInput]) input.value = "";
    ruleQuality.value = "";
    ruleConnections.value = "";
    ruleChecksum.checked = false;
    agentStatus(`${rules[rules.length - 1]?.label ?? "The rule"} added.`);
  } catch (error) {
    showError(settingsError, error);
  }
});

ruleTest.addEventListener("click", async () => {
  clearError(settingsError);
  ruleTestResult.replaceChildren();
  const url = ruleTestLink.value.trim();
  if (!url) return;
  try {
    const advice = await invoke<RuleAdvice>("inspect_rules", { url });
    for (const line of advice.lines) {
      const item = document.createElement("li");
      item.textContent = line.trim();
      ruleTestResult.append(item);
    }
  } catch (error) {
    showError(settingsError, error);
  }
});

ruleTestLink.addEventListener("keydown", (event) => {
  if (event.key === "Enter") {
    event.preventDefault();
    ruleTest.click();
  }
});

/* AI agents (FP-066) ---------------------------------------------------------
   The engine keeps each agent's folders and limits and enforces them; this
   only edits them. Taking a folder away, or revoking an agent, makes its
   unfinished downloads outside the remaining folders wait for approval. */

const agentsList = required<HTMLDivElement>("agents-list");
const agentNewName = required<HTMLInputElement>("agent-new-name");
const agentAdd = required<HTMLButtonElement>("agent-add");
const DEFAULT_AGENT_BYTES = 1024 * 1024 * 1024;
const DEFAULT_AGENT_PER_HOUR = 20;
const MIB = 1024 * 1024;
let agents: AgentView[] = [];

async function refreshAgents(): Promise<void> {
  try {
    agents = await invoke<AgentView[]>("list_agents");
    renderAgents();
  } catch (error) {
    showError(settingsError, error);
  }
}

function agentStatus(message: string): void {
  settingsStatus.textContent = message;
  settingsStatus.hidden = false;
  announce(message);
}

/** Saves one agent and redraws, putting focus back where the person was. */
async function saveAgent(agent: AgentView, message: string, focus: string): Promise<void> {
  clearError(settingsError);
  try {
    agents = await invoke<AgentView[]>("set_agent", {
      name: agent.name,
      folders: agent.folders,
      maxBytes: agent.maxBytes,
      maxNewJobsPerHour: agent.maxNewJobsPerHour,
    });
    renderAgents();
    agentStatus(message);
  } catch (error) {
    showError(settingsError, error);
  }
  agentsList.querySelector<HTMLElement>(focus)?.focus();
}

function agentButton(label: string, name: string, action: string, accessible: string, danger = false): HTMLButtonElement {
  const button = document.createElement("button");
  button.type = "button";
  button.className = danger ? "secondary danger" : "secondary";
  button.textContent = label;
  button.dataset.agent = name;
  button.dataset.agentAction = action;
  button.setAttribute("aria-label", accessible);
  return button;
}

function renderAgents(): void {
  agentsList.replaceChildren();
  if (!agents.length) {
    const empty = document.createElement("p");
    empty.className = "field-note";
    empty.textContent = "No agent has access yet. Every request from an agent waits for you.";
    agentsList.append(empty);
    return;
  }
  for (const agent of agents) {
    const card = document.createElement("section");
    card.className = "agent-card";
    card.setAttribute("aria-labelledby", `agent-${agent.name}`);
    const heading = document.createElement("h3");
    heading.id = `agent-${agent.name}`;
    heading.textContent = agent.name;
    const summary = document.createElement("p");
    summary.className = "hint field-note";
    summary.textContent = agent.folders.length
      ? `Saves into ${agent.folders.length === 1 ? "this folder" : "these folders"} without asking, up to ${formatBytes(agent.maxBytes)} a download and ${agent.maxNewJobsPerHour} downloads an hour.`
      : "No folders yet: everything it asks for waits for you.";
    card.append(heading, summary);

    if (agent.folders.length) {
      const list = document.createElement("ul");
      list.className = "agent-folders";
      list.setAttribute("aria-label", `Folders for ${agent.name}`);
      agent.folders.forEach((folder, index) => {
        const item = document.createElement("li");
        const path = document.createElement("code");
        path.textContent = folder;
        const remove = agentButton("Remove", agent.name, "remove-folder", `Remove ${folder} from ${agent.name}`);
        remove.dataset.index = String(index);
        item.append(path, remove);
        list.append(item);
      });
      card.append(list);
    }

    const limits = document.createElement("div");
    limits.className = "agent-limits";
    const sizeLabel = document.createElement("label");
    sizeLabel.textContent = "Largest download (MB)";
    const size = document.createElement("input");
    size.type = "number";
    size.min = "1";
    size.value = String(Math.max(1, Math.round(agent.maxBytes / MIB)));
    size.id = `agent-size-${agent.name}`;
    sizeLabel.htmlFor = size.id;
    const sizeWrap = document.createElement("div");
    sizeWrap.append(sizeLabel, size);
    const rateLabel = document.createElement("label");
    rateLabel.textContent = "Downloads an hour";
    const rate = document.createElement("input");
    rate.type = "number";
    rate.min = "1";
    rate.max = "1000";
    rate.value = String(agent.maxNewJobsPerHour);
    rate.id = `agent-rate-${agent.name}`;
    rateLabel.htmlFor = rate.id;
    const rateWrap = document.createElement("div");
    rateWrap.append(rateLabel, rate);
    limits.append(sizeWrap, rateWrap, agentButton("Save limits", agent.name, "save-limits", `Save limits for ${agent.name}`));
    card.append(limits);

    const actions = document.createElement("div");
    actions.className = "agent-actions";
    actions.append(
      agentButton("Add folder…", agent.name, "add-folder", `Add a folder for ${agent.name}`),
      agentButton("Revoke access", agent.name, "revoke", `Revoke access for ${agent.name}`, true),
    );
    card.append(actions);
    agentsList.append(card);
  }
}

agentsList.addEventListener("click", async (event) => {
  const button = (event.target as HTMLElement).closest<HTMLButtonElement>("button[data-agent-action]");
  if (!button) return;
  const name = button.dataset.agent!;
  const agent = agents.find((candidate) => candidate.name === name);
  if (!agent) return;
  const here = (action: string) => `button[data-agent="${CSS.escape(name)}"][data-agent-action="${action}"]`;
  switch (button.dataset.agentAction) {
    case "add-folder": {
      clearError(settingsError);
      try {
        const selected = await open({ directory: true, title: `Choose a folder ${name} may save into` });
        if (typeof selected !== "string") return;
        if (agent.folders.some((folder) => folder.toLowerCase() === selected.toLowerCase())) {
          agentStatus(`${name} can already save into ${selected}.`);
          return;
        }
        await saveAgent({ ...agent, folders: [...agent.folders, selected] }, `${name} can now save into ${selected}.`, here("add-folder"));
      } catch (error) {
        showError(settingsError, error);
      }
      return;
    }
    case "remove-folder": {
      const index = Number(button.dataset.index);
      const removed = agent.folders[index];
      const folders = agent.folders.filter((_, position) => position !== index);
      await saveAgent(
        { ...agent, folders },
        `${name} can no longer save into ${removed}. Its downloads there wait for your approval.`,
        here("add-folder"),
      );
      return;
    }
    case "save-limits": {
      const size = Number(required<HTMLInputElement>(`agent-size-${name}`).value);
      const rate = Number(required<HTMLInputElement>(`agent-rate-${name}`).value);
      if (!(size >= 1) || !(rate >= 1) || !Number.isInteger(rate)) {
        showError(settingsError, "Give a size of at least 1 MB and a whole number of downloads an hour.");
        return;
      }
      await saveAgent(
        { ...agent, maxBytes: Math.round(size * MIB), maxNewJobsPerHour: Math.min(rate, 1000) },
        `Limits for ${name} saved.`,
        here("save-limits"),
      );
      return;
    }
    case "revoke": {
      clearError(settingsError);
      try {
        agents = await invoke<AgentView[]>("revoke_agent", { name });
        renderAgents();
        agentStatus(`${name} no longer has access. Anything it had not finished waits for your approval.`);
        agentNewName.focus();
      } catch (error) {
        showError(settingsError, error);
      }
      return;
    }
  }
});

agentAdd.addEventListener("click", async () => {
  const name = agentNewName.value.trim();
  if (!name) {
    showError(settingsError, "Type the agent's name, as given to fetchpath mcp --agent.");
    agentNewName.focus();
    return;
  }
  if (agents.some((agent) => agent.name === name)) {
    agentStatus(`${name} is already listed.`);
    return;
  }
  agentNewName.value = "";
  await saveAgent(
    { name, folders: [], maxBytes: DEFAULT_AGENT_BYTES, maxNewJobsPerHour: DEFAULT_AGENT_PER_HOUR },
    `${name} added. Give it a folder so its downloads there start without asking.`,
    `button[data-agent="${CSS.escape(name)}"][data-agent-action="add-folder"]`,
  );
});

agentNewName.addEventListener("keydown", (event) => {
  if (event.key === "Enter") {
    event.preventDefault();
    agentAdd.click();
  }
});

const cliState = required<HTMLParagraphElement>("cli-state");
const cliDetail = required<HTMLParagraphElement>("cli-detail");
const cliPath = required<HTMLElement>("cli-path");

async function refreshCliStatus(): Promise<void> {
  const path = await invoke<string | null>("cli_path").catch(() => null);
  cliPath.hidden = !path;
  cliPath.textContent = path ?? "";
  if (path) {
    cliState.textContent = "The fetchpath command is installed.";
    cliDetail.textContent =
      "Open a new terminal and type fetchpath --help. Downloads started there run on their own and do not appear in this list.";
  } else {
    cliState.textContent = "The fetchpath command is not installed beside this copy of Fetchpath.";
    cliDetail.textContent = "The Fetchpath installer adds it. A copy run from elsewhere, such as a development build, does not.";
  }
}

/* Add download -------------------------------------------------------------
   The composer lives in a modal dialog so the queue can be the whole window.
   Closing it keeps whatever was typed; Cancel clears it. */

function openComposer(prefill?: string): void {
  if (prefill !== undefined) {
    if (!editingJobId) urlInput.value = prefill;
    urlInput.dispatchEvent(new Event("input"));
  }
  if (!addDialog.open) openDialog(addDialog, urlInput);
  else urlInput.focus();
  urlInput.select();
}

addOpenButton.addEventListener("click", () => openComposer());
emptyAddButton.addEventListener("click", () => openComposer());
addCancelButton.addEventListener("click", () => {
  clearComposer();
  addDialog.close();
});
addDialog.addEventListener("close", () => {
  // A half-made correction is not kept: reopening starts a new download.
  if (editingJobId) clearComposer();
  // The draft and its error stay in the dialog for next time, but an alert
  // about a form that is no longer on screen would only confuse.
  liveAlert.textContent = "";
  announcedAlert = "";
  restoreDialogFocus(addOpenButton);
});

/**
 * Pasting a link anywhere outside a text field opens Add download with it,
 * the way people already move links from a browser.
 */
document.addEventListener("paste", (event) => {
  const target = event.target as HTMLElement | null;
  if (target?.closest("input, textarea, select, [contenteditable]")) return;
  if (settingsDialog.open || shortcutsDialog.open) return;
  const text = event.clipboardData?.getData("text")?.trim() ?? "";
  const links = text.split(/\s+/).filter((part) => /^https?:\/\//i.test(part));
  if (!links.length) return;
  event.preventDefault();
  openComposer(links.join("\n"));
});

/* Browser extension ------------------------------------------------------- */

async function refreshBrowserSetup(): Promise<void> {
  try {
    renderBrowserSetup(await invoke<BrowserSetupStatus>("browser_setup_status"));
  } catch (error) {
    browserSetupState.textContent = "Could not check the browser extension.";
    browserSetupDetail.textContent = String(error);
  }
}

function renderBrowserSetup(status: BrowserSetupStatus): void {
  // Firefox's host is registered for a future signed add-on, but it cannot load
  // this extension yet, so it is not named as connected.
  const registered = status.browsers
    .filter((entry) => entry.registered && entry.browser !== "Firefox")
    .map((entry) => entry.browser);
  const ready = registered.length > 0 && status.extensionDir !== null;
  browserSetupState.dataset.ready = String(ready);
  browserSetupState.textContent = ready
    ? `Fetchpath is connected to ${registered.join(" and ")}. Add the extension to your browser to send links.`
    : "The browser connection is set up by the Fetchpath installer.";
  browserSetupDetail.textContent = ready
    ? "Firefox is not supported yet: it only accepts signed add-ons."
    : "This copy was not installed with the installer, so browser capture is unavailable here.";
  browserSetupSteps.hidden = !ready;
  browserExtensionPath.textContent = status.extensionDir ?? "";
  for (const button of [browserOpenFolder, browserCopyFolder, browserCopyChrome, browserCopyEdge]) button.hidden = !ready;
}

browserOpenFolder.addEventListener("click", async () => {
  clearError(settingsError);
  try {
    await invoke("reveal_extension_folder");
    announce("Opened the extension folder in File Explorer.");
  } catch (error) {
    showError(settingsError, error);
  }
});

browserCopyFolder.addEventListener("click", async () => {
  const folder = browserExtensionPath.textContent ?? "";
  if (!folder) return;
  await navigator.clipboard.writeText(folder);
  settingsStatus.textContent = "Copied the extension folder. Paste it into the folder picker's address bar.";
  settingsStatus.hidden = false;
  announce(settingsStatus.textContent);
});

for (const [button, address, browser] of [
  [browserCopyChrome, "chrome://extensions", "Chrome"],
  [browserCopyEdge, "edge://extensions", "Edge"],
] as const) {
  button.addEventListener("click", async () => {
    // Browsers refuse to open their internal pages from another program, so
    // the address is copied for the person to paste instead.
    await navigator.clipboard.writeText(address);
    settingsStatus.textContent = `Copied ${address}. Paste it into ${browser}'s address bar.`;
    settingsStatus.hidden = false;
    announce(settingsStatus.textContent);
  });
}

/* Settings ---------------------------------------------------------------- */

async function loadSettings(): Promise<void> {
  try {
    settingsView = await invoke<SettingsView>("get_settings");
    applySettingsToForm(settingsView);
  } catch (error) {
    showError(settingsError, error);
  }
}

function applySettingsToForm(view: SettingsView): void {
  const { settings } = view;
  systemDownloadDir = view.systemDownloadDir;
  defaultDestinationDir = settings.defaultDestinationDir ?? view.systemDownloadDir;

  settingsRepaired.hidden = !view.repaired;
  concurrencyInput.max = String(view.maxActiveLimit);
  concurrencyInput.value = String(settings.maxActiveDownloads);
  concurrencyValue.textContent = String(settings.maxActiveDownloads);
  retryAttemptsInput.max = String(view.maxRetryAttempts);
  retryAttemptsInput.value = String(settings.autoRetryMaxAttempts);
  retryAttemptsValue.textContent = String(settings.autoRetryMaxAttempts);
  retryAttemptsInput.disabled = !settings.autoRetry;

  settingDestination.value = settings.defaultDestinationDir ?? "";
  settingDestination.placeholder = view.systemDownloadDir ?? "Your Windows Downloads folder";
  settingClearDestination.hidden = !settings.defaultDestinationDir;

  autoRetryInput.checked = settings.autoRetry;
  closeToTrayInput.checked = settings.closeToTray;
  confirmRemoveInput.checked = settings.confirmRemoveCompleted;
  powerModeInput.checked = settings.powerMode;
  themeSelect.value = settings.theme;

  applyTheme(settings.theme);
  statsPanel.hidden = !settings.powerMode;
  onboarding.hidden = settings.onboardingCompleted;
  trayNote.textContent = settings.closeToTray
    ? "Closing this window keeps the queue available from the notification area."
    : "Closing this window stops Fetchpath. Downloads in progress are paused at their last checkpoint.";
}

/** Windows' own setting decides unless the user chose a specific appearance. */
function applyTheme(theme: Settings["theme"]): void {
  if (theme === "system") document.documentElement.removeAttribute("data-theme");
  else document.documentElement.setAttribute("data-theme", theme);
}

/** Sends one changed field, then re-applies whatever the backend accepted. */
async function changeSetting(patch: Partial<Settings>, note?: string): Promise<void> {
  if (!settingsView) return;
  clearError(settingsError);
  const next = { ...settingsView.settings, ...patch };
  try {
    settingsView = await invoke<SettingsView>("update_settings", { next });
    applySettingsToForm(settingsView);
    if (note) {
      settingsStatus.textContent = note;
      settingsStatus.hidden = false;
      announce(note);
    }
    await refreshQueue();
  } catch (error) {
    showError(settingsError, error);
    // The backend refused, so the control must go back to the stored value
    // rather than showing a change that did not happen.
    applySettingsToForm(settingsView);
  }
}

concurrencyInput.addEventListener("input", () => {
  concurrencyValue.textContent = concurrencyInput.value;
});
concurrencyInput.addEventListener("change", () => {
  void changeSetting({ maxActiveDownloads: Number(concurrencyInput.value) });
});

retryAttemptsInput.addEventListener("input", () => {
  retryAttemptsValue.textContent = retryAttemptsInput.value;
});
retryAttemptsInput.addEventListener("change", () => {
  void changeSetting({ autoRetryMaxAttempts: Number(retryAttemptsInput.value) });
});

autoRetryInput.addEventListener("change", () => void changeSetting({ autoRetry: autoRetryInput.checked }));
closeToTrayInput.addEventListener("change", () => void changeSetting({ closeToTray: closeToTrayInput.checked }));
confirmRemoveInput.addEventListener("change", () =>
  void changeSetting({ confirmRemoveCompleted: confirmRemoveInput.checked }),
);
powerModeInput.addEventListener("change", () => void changeSetting({ powerMode: powerModeInput.checked }));
themeSelect.addEventListener("change", () => void changeSetting({ theme: themeSelect.value as Settings["theme"] }));

settingChooseDestination.addEventListener("click", async () => {
  clearError(settingsError);
  try {
    const selected = await open({
      directory: true,
      title: "Choose the default save folder",
      defaultPath: settingDestination.value || systemDownloadDir || undefined,
    });
    if (typeof selected === "string") {
      await changeSetting({ defaultDestinationDir: selected }, "Default save folder updated.");
    }
  } catch (error) {
    showError(settingsError, error);
  }
});

settingClearDestination.addEventListener("click", () => {
  void changeSetting({ defaultDestinationDir: null }, "New downloads will use your Windows Downloads folder.");
});

dismissOnboarding.addEventListener("click", () => {
  onboarding.hidden = true;
  void changeSettingWithoutDialog({ onboardingCompleted: true });
  addOpenButton.focus({ preventScroll: true });
});

onboardingOpenMedia.addEventListener("click", () => void showSettings("integrations"));
mediaOpenSettings.addEventListener("click", () => void showSettings("integrations"));

/** Settings changed from outside the dialog, where there is no form to update. */
async function changeSettingWithoutDialog(patch: Partial<Settings>): Promise<void> {
  if (!settingsView) return;
  try {
    settingsView = await invoke<SettingsView>("update_settings", { next: { ...settingsView.settings, ...patch } });
    applySettingsToForm(settingsView);
  } catch {
    // A failed preference write is not worth interrupting the queue for; the
    // panel will show the stored value again the next time it opens.
  }
}

/* Media tools ------------------------------------------------------------- */

async function refreshToolsStatus(): Promise<void> {
  try {
    toolsStatus = await invoke<ToolsStatus>("media_tools_status");
    renderToolsStatus(toolsStatus);
  } catch (error) {
    showError(settingsError, error);
  }
  updateMediaAvailability();
}

function renderToolsStatus(status: ToolsStatus): void {
  mediaToolsState.dataset.ready = String(status.ready);
  if (status.ready) {
    mediaToolsState.textContent = "Ready to save video and audio";
    mediaToolsDetail.textContent = [status.ytDlpVersion, status.ffmpegVersion]
      .filter(Boolean)
      .join(" · ") || status.installDir;
  } else if (status.problem) {
    mediaToolsState.textContent = "Found, but they would not run";
    mediaToolsDetail.textContent = status.problem;
  } else {
    mediaToolsState.textContent = "Not set up yet";
    mediaToolsDetail.textContent =
      "Fetchpath does not include yt-dlp or ffmpeg. Download them here, or point Fetchpath at a folder that already has them.";
  }

  // Only offer the download when this build actually carries a checksum to
  // check the result against. Otherwise the honest offer is the manual one.
  const unpinned = status.available.filter((tool) => !tool.pinned);
  mediaToolsInstall.disabled = status.ready || unpinned.length > 0;
  mediaToolsInstall.textContent = status.ready ? "Already set up" : "Download and set up";

  if (unpinned.length && !status.ready) {
    mediaToolsLicence.textContent =
      `This build has no recorded checksum for ${unpinned.map((tool) => tool.name).join(" and ")}, ` +
      "so Fetchpath will not download it. Install it yourself and choose the folder below.";
  } else {
    mediaToolsLicence.textContent = status.available
      .map((tool) => `${tool.name} ${tool.version} — ${tool.license}`)
      .join(" · ");
  }
}

mediaToolsInstall.addEventListener("click", async () => {
  clearError(settingsError);
  mediaToolsInstall.disabled = true;
  mediaToolsInstall.textContent = "Downloading…";
  announce("Downloading the media tools. This can take a minute.");
  try {
    toolsStatus = await invoke<ToolsStatus>("install_media_tools");
    renderToolsStatus(toolsStatus);
    announce(toolsStatus.ready ? "Media tools are ready." : "The media tools were not set up.");
  } catch (error) {
    showError(settingsError, error);
    if (toolsStatus) renderToolsStatus(toolsStatus);
  } finally {
    updateMediaAvailability();
    if (mediaToolsInstall.isConnected && !mediaToolsInstall.disabled) {
      mediaToolsInstall.focus({ preventScroll: true });
    }
  }
});

mediaToolsLocate.addEventListener("click", async () => {
  clearError(settingsError);
  try {
    const selected = await open({
      directory: true,
      title: "Choose the folder that holds yt-dlp and ffmpeg",
      defaultPath: toolsStatus?.installDir,
    });
    if (typeof selected !== "string") return;
    toolsStatus = await invoke<ToolsStatus>("use_media_tools_dir", { directory: selected });
    renderToolsStatus(toolsStatus);
    updateMediaAvailability();
    announce(toolsStatus.ready ? "Media tools are ready." : "Those programs could not be used.");
  } catch (error) {
    showError(settingsError, error);
  }
});

/** Swaps the composer's media section between "set these up" and "inspect". */
function updateMediaAvailability(): void {
  const ready = toolsStatus?.ready ?? false;
  mediaSetupNeeded.hidden = ready;
  mediaInspectControls.hidden = !ready;
}

/* Composer ---------------------------------------------------------------- */

kindChooser.addEventListener("change", () => {
  mediaInspection = null;
  inspectedMediaUrl = "";
  mediaQuality.replaceChildren(new Option("Inspect a link first", ""));
  mediaQuality.disabled = true;
  mediaTitle.textContent = "";
  const media = downloadKind() === "media";
  const torrent = downloadKind() === "torrent";
  mediaOptions.hidden = !media;
  torrentOptions.hidden = !torrent;
  // A checksum describes one file's bytes; a media variant is assembled
  // locally, so there is nothing a published checksum could describe.
  checksumField.hidden = media || torrent;
  urlInput.rows = media || torrent ? 2 : 3;
  urlInput.placeholder = torrent ? "magnet:?xt=urn:btih:…" : media ? "https://example.com/watch/…" : "https://example.com/archive.zip";
  // A torrent needs no Save as: left empty, the engine picks the folder.
  destinationInput.placeholder = torrent ? "Optional: a new folder for this torrent" : "Choose a destination file";
  destinationInput.required = !torrent;
  destinationLabel.textContent = torrent ? "Folder (optional)" : "Save as";
  if (torrent && destinationIsSuggested) destinationInput.value = "";
  if (media && !toolsStatus) void refreshToolsStatus();
  renderPreview();
});

torrentPickFile.addEventListener("click", async () => {
  clearError(formError);
  try {
    const selected = await open({
      title: "Choose a .torrent file",
      multiple: false,
      filters: [{ name: "Torrent metadata", extensions: ["torrent"] }],
    });
    if (typeof selected !== "string") return;
    urlInput.value = selected;
    analyzedUrl = selected;
    destinationIsSuggested = true;
    destinationInput.value = "";
    renderPreview();
    destinationInput.focus();
  } catch (error) {
    showError(formError, error);
  }
});

inspectMediaButton.addEventListener("click", () => {
  clearError(formError);
  const urls = parseUrls();
  if (urls.length !== 1) {
    showError(formError, "Enter one media address to inspect.");
    return;
  }
  void inspectMedia(urls[0], false);
});

/**
 * Inspects one media link and fills the quality list. `quiet` is the automatic
 * path: a failure leaves the form as it was instead of showing an error, so a
 * link that turns out not to be media simply stays a file.
 */
async function inspectMedia(url: string, quiet: boolean): Promise<MediaInspection | null> {
  const wasFocused = document.activeElement === inspectMediaButton;
  inspectMediaButton.disabled = true;
  inspectMediaButton.textContent = "Inspecting…";
  if (!quiet || downloadKind() === "media") mediaTitle.textContent = "Checking the available qualities…";
  announce("Inspecting the media link.");
  try {
    const inspection = await invoke<MediaInspection>("inspect_media", { url });
    // The link may have changed while the helper ran; a late answer for an
    // old link must not overwrite the current one.
    if (parseUrls()[0] !== url) return null;
    return inspection;
  } catch (error) {
    if (parseUrls()[0] !== url) return null;
    mediaInspection = null;
    inspectedMediaUrl = "";
    if (!quiet) {
      mediaTitle.textContent = "";
      showError(formError, error);
    } else if (downloadKind() === "media") {
      mediaTitle.textContent = "No video or audio was found at this link.";
    }
    // The helpers may have gone missing since the last check; re-reading the
    // status turns a repeated failure into the setup path.
    void refreshToolsStatus();
    return null;
  } finally {
    inspectMediaButton.disabled = false;
    inspectMediaButton.textContent = "Inspect again";
    if (wasFocused) inspectMediaButton.focus({ preventScroll: true });
  }
}

/** Shows an inspection and picks the best quality up to 1080p by default. */
function applyInspection(url: string, inspection: MediaInspection): void {
  mediaInspection = inspection;
  inspectedMediaUrl = url;
  mediaQuality.replaceChildren(...inspection.variants.map((variant) => new Option(variant.label, variant.id)));
  mediaQuality.disabled = false;
  const preferred =
    inspection.variants.find((variant) => variant.kind === "video" && (variant.height ?? 0) <= 1080) ??
    inspection.variants[0];
  mediaQuality.value = preferred.id;
  mediaTitle.textContent = inspection.durationSeconds
    ? `${inspection.title} · ${formatDuration(inspection.durationSeconds)}`
    : inspection.title;
  setMediaDestination(preferred, true);
  renderPreview();
  announce(`${inspection.title}. ${preferred.label} selected; ${inspection.variants.length} qualities available.`);
}

/* Automatic link analysis --------------------------------------------------
   People paste a link; deciding whether it is a file or a video is
   Fetchpath's job. The radio buttons remain as an override, and a choice made
   there is respected for the rest of that draft. */

const MEDIA_HOSTS = [
  "youtube.com", "youtu.be", "vimeo.com", "dailymotion.com", "dai.ly", "twitch.tv", "tiktok.com",
  "instagram.com", "facebook.com", "fb.watch", "x.com", "twitter.com", "soundcloud.com", "bandcamp.com",
  "bilibili.com", "rumble.com", "odysee.com", "streamable.com", "ted.com", "reddit.com", "v.redd.it",
  "mixcloud.com", "nicovideo.jp", "archive.org",
];
const FILE_EXTENSIONS = new Set([
  "7z", "aab", "apk", "appx", "bin", "bz2", "cab", "csv", "deb", "dmg", "doc", "docx", "epub", "exe", "flac",
  "gz", "img", "iso", "jar", "json", "m4a", "mkv", "mobi", "mov", "mp3", "mp4", "msi", "msix", "ogg", "pdf",
  "pkg", "ppt", "pptx", "rar", "rpm", "svg", "tar", "tgz", "txt", "wav", "webm", "xls", "xlsx",
  "xml", "xz", "zip", "zst", "png", "jpg", "jpeg", "gif", "webp",
]);

type LinkKind = "file" | "media" | "torrent" | "unknown";

function classifyLink(value: string): LinkKind {
  if (/^magnet:\?/i.test(value.trim())) return "torrent";
  if (/^(?:[a-zA-Z]:[\\/]|\\\\).*\.torrent$/i.test(value.trim())) return "torrent";
  let url: URL;
  try {
    url = new URL(value);
  } catch {
    return "unknown";
  }
  const host = url.hostname.toLowerCase().replace(/^www\.|^m\.|^music\./, "");
  const lastSegment = url.pathname.split("/").pop() ?? "";
  const extension = lastSegment.includes(".") ? lastSegment.split(".").pop()!.toLowerCase() : "";
  if (url.protocol === "https:" && extension === "torrent") return "torrent";
  // A direct file on a media site (an .mp4 on archive.org) is still a file.
  if (FILE_EXTENSIONS.has(extension)) return "file";
  if (MEDIA_HOSTS.some((media) => host === media || host.endsWith(`.${media}`))) return "media";
  return "unknown";
}

let kindChosenByPerson = false;
let settingKindAutomatically = false;
let analysisTimer = 0;
let analyzedUrl = "";

function setKind(kind: "file" | "media" | "torrent"): void {
  if (downloadKind() === kind) return;
  const radio = form.querySelector<HTMLInputElement>(`input[name="download-kind"][value="${kind}"]`);
  if (!radio) return;
  radio.checked = true;
  settingKindAutomatically = true;
  kindChooser.dispatchEvent(new Event("change"));
  settingKindAutomatically = false;
  // The suggestion ran for the old kind when the link was typed.
  if (kind === "file") suggestDestination();
}

kindChooser.addEventListener("change", () => {
  if (!settingKindAutomatically) kindChosenByPerson = true;
});

const repositoryNote = required<HTMLParagraphElement>("repository-note");
let repository: RepositoryView | null = null;
let repositoryUrl = "";

function isRepositoryLink(value: string): boolean {
  return /^(hf:\/\/|https:\/\/(www\.)?huggingface\.co\/)/i.test(value.trim());
}

function showRepository(view: RepositoryView | null, problem?: string): void {
  repository = view;
  if (!view) {
    repositoryNote.hidden = !problem;
    repositoryNote.textContent = problem ?? "";
    return;
  }
  const checked = view.files.filter((file) => file.sha256).length;
  const name = view.repo.split("/").pop() ?? view.repo;
  repositoryNote.textContent =
    `Hugging Face ${view.kind} ${view.repo} at commit ${view.commit.slice(0, 12)}: ` +
    `${view.files.length} ${view.files.length === 1 ? "file" : "files"}, ${formatBytes(view.total_bytes)}, ` +
    `${checked} checked against the SHA-256 Hugging Face states. They go in a folder named ${name} inside the folder below.` +
    (view.skipped?.length ? ` ${view.skipped.length} left out because Windows can't save their names.` : "");
  repositoryNote.hidden = false;
  if (destinationIsSuggested) destinationInput.value = ruleFolder ?? defaultDestinationDir ?? "";
}

async function analyzeRepository(url: string): Promise<void> {
  setKind("file");
  repositoryUrl = url;
  showRepository(null, "Looking up this repository…");
  try {
    const view = await invoke<RepositoryView>("inspect_repository", { url });
    if (repositoryUrl === url) showRepository(view);
  } catch (error) {
    if (repositoryUrl === url) showRepository(null, String(error));
  }
}

async function analyzeLink(): Promise<void> {
  const urls = parseUrls();
  if (editingJobId || urls.length !== 1) return;
  const url = urls[0];
  if (url === analyzedUrl) return;
  analyzedUrl = url;
  if (isRepositoryLink(url)) {
    await analyzeRepository(url);
    return;
  }
  if (repository || repositoryUrl) {
    repositoryUrl = "";
    showRepository(null);
  }
  const ready = toolsStatus?.ready ?? false;
  if (kindChosenByPerson) {
    if (downloadKind() === "media" && ready) {
      const inspection = await inspectMedia(url, true);
      if (inspection) applyInspection(url, inspection);
    }
    return;
  }
  const kind = classifyLink(url);
  if (kind === "file") {
    setKind("file");
    return;
  }
  if (kind === "torrent") {
    setKind("torrent");
    return;
  }
  if (kind === "media") {
    // Without the helpers this shows the setup step instead of a dead end.
    setKind("media");
    if (!ready) return;
  } else if (!ready) {
    return;
  }
  const inspection = await inspectMedia(url, true);
  if (!inspection) return;
  setKind("media");
  applyInspection(url, inspection);
}

urlInput.addEventListener("input", () => {
  if (!urlInput.value.trim()) {
    kindChosenByPerson = false;
    analyzedUrl = "";
  }
  window.clearTimeout(analysisTimer);
  analysisTimer = window.setTimeout(() => {
    void analyzeLink();
    void adviseRules();
  }, 400);
});

/** Says which of the person's rules decides for the first link, and why, and
 *  proposes its folder while the destination is still Fetchpath's own. */
async function adviseRules(): Promise<void> {
  const first = parseUrls()[0] ?? "";
  if (first === advisedUrl) return;
  advisedUrl = first;
  if (!first || editingJobId) {
    showRuleAdvice(null);
    return;
  }
  const advice = await invoke<RuleAdvice>("inspect_rules", { url: first }).catch(() => null);
  if (advisedUrl !== first) return; // a newer link was typed meanwhile
  showRuleAdvice(advice);
}

function showRuleAdvice(advice: RuleAdvice | null): void {
  ruleFolder = advice?.matched ? advice.folder : null;
  if (!advice?.matched) {
    ruleNote.hidden = true;
    ruleNote.textContent = "";
    if (destinationIsSuggested) suggestDestination();
    return;
  }
  const parts = [advice.matched];
  if (advice.folder) {
    parts.push(destinationIsSuggested ? `saves in ${advice.folder}` : `would save in ${advice.folder}; your choice is used`);
  }
  if (advice.needsChecksum) parts.push("needs a checksum, under Advanced options");
  ruleNote.textContent = `${parts.join(" · ")}.`;
  ruleNote.hidden = false;
  if (destinationIsSuggested) {
    const variant = selectedMediaVariant();
    if (downloadKind() === "media" && variant) setMediaDestination(variant);
    else suggestDestination();
  }
}

mediaQuality.addEventListener("change", () => {
  const variant = selectedMediaVariant();
  if (variant) setMediaDestination(variant, true);
  renderPreview();
});

chooseButton.addEventListener("click", async () => {
  clearError(formError);
  try {
    const suggested = destinationInput.value || suggestedFilename(parseUrls()[0] ?? "");
    const selected = await save({
      title: downloadKind() === "torrent" ? "Choose a new torrent folder name" : "Save download as",
      // Opens in the user's chosen folder rather than wherever Windows last
      // left the picker.
      defaultPath: suggested.includes("\\") || suggested.includes("/")
        ? suggested
        : joinPath(defaultDestinationDir, suggested),
    });
    if (selected) {
      destinationInput.value = selected;
      destinationIsSuggested = false;
      renderPreview();
    }
  } catch (error) {
    showError(formError, error);
  }
});

clearScheduleButton.addEventListener("click", () => {
  scheduleInput.value = "";
  renderPreview();
  scheduleInput.focus();
});

for (const eventName of ["input", "change"] as const) {
  urlInput.addEventListener(eventName, renderPreview);
  destinationInput.addEventListener(eventName, renderPreview);
  scheduleInput.addEventListener(eventName, renderPreview);
}

urlInput.addEventListener("input", () => {
  if (downloadKind() === "media" && parseUrls()[0] !== inspectedMediaUrl) {
    mediaInspection = null;
    mediaQuality.replaceChildren(new Option("Inspect this link to continue", ""));
    mediaQuality.disabled = true;
    mediaTitle.textContent = "";
  }
  suggestDestination();
});

/**
 * Fills the destination in from the first address, so the common case needs no
 * picker at all.
 *
 * Two rules keep the suggestion from getting in the way. It only runs while the
 * destination is still Fetchpath's own suggestion — the moment the user edits
 * it or picks a folder, it is theirs and is never overwritten. And it only runs
 * once the address parses as a real URL: recomputing on every keystroke would
 * settle on `download.bin` from the first character typed and then never
 * correct itself, because from the second character the field is no longer
 * empty.
 */
function suggestDestination(): void {
  if (!destinationIsSuggested || downloadKind() !== "file") return;
  const first = parseUrls()[0];
  if (!first) {
    destinationInput.value = "";
    return;
  }
  let parsed: URL;
  try {
    parsed = new URL(first);
  } catch {
    // Not a usable address yet. Leave whatever the last good suggestion was
    // rather than replacing it with a placeholder.
    return;
  }
  if (!parsed.protocol) return;
  // A repository goes in a folder named after it inside this one.
  if (isRepositoryLink(first)) {
    destinationInput.value = ruleFolder ?? defaultDestinationDir ?? "";
    return;
  }
  destinationInput.value = joinPath(ruleFolder ?? defaultDestinationDir, suggestedFilename(first));
}

// Any edit to the destination, by typing or by picking, makes it the user's.
for (const eventName of ["input", "change"] as const) {
  destinationInput.addEventListener(eventName, () => {
    destinationIsSuggested = false;
  });
}

form.addEventListener("submit", async (event) => {
  event.preventDefault();
  clearError(formError);
  const drafts = buildDrafts();
  if (!editingJobId && !drafts.length) {
    showError(formError, "Add at least one download address.");
    return;
  }
  setComposerAvailability(false, editingJobId ? "Saving…" : "Adding…");
  try {
    if (editingJobId) {
      const urls = parseUrls();
      if (urls.length > 1) throw new Error("Edit one recovery item at a time.");
      // Correcting only the checksum keeps the link already on record, which
      // may carry private values that were never shown.
      if (!urls.length && !editingChecksum) throw new Error("Paste a refreshed address to continue.");
      await invoke<JobSnapshot>("retry_download", {
        jobId: editingJobId,
        url: urls[0] ?? null,
        destination: destinationInput.value.trim() || null,
        checksum: checksumInput.value,
      });
      editingJobId = null;
      editingChecksum = false;
    } else if (repository && drafts.length === 1 && repositoryUrl === drafts[0].url) {
      const folder = destinationInput.value.trim();
      if (!folder) throw new Error("Choose the folder the repository goes in.");
      const added = await invoke<JobSnapshot[]>("add_repository", { url: drafts[0].url, folder });
      drafts.length = added.length;
    } else if (downloadKind() === "media") {
      const variant = selectedMediaVariant();
      if (!variant || inspectedMediaUrl !== drafts[0].url) {
        throw new Error("Inspect this media link and choose a quality first.");
      }
      await invoke<JobSnapshot>("start_media_download", {
        draft: {
          url: drafts[0].url,
          variantId: variant.id,
          qualityLabel: variant.label,
          destination: drafts[0].destination,
          notBeforeMs: drafts[0].notBeforeMs,
        },
      });
    } else if (downloadKind() === "torrent") {
      if (drafts.length !== 1) throw new Error("Add one torrent at a time.");
      if (!torrentDiscovery.checked) throw new Error("Choose whether this torrent may contact peers and discovery services.");
      await invoke<JobSnapshot>("start_torrent_download", {
        draft: {
          url: drafts[0].url,
          destination: drafts[0].destination,
          notBeforeMs: drafts[0].notBeforeMs,
          discoverPeers: torrentDiscovery.checked,
          upload: torrentUpload.checked,
        },
      });
    } else {
      await invoke<JobSnapshot[]>("start_batch", { drafts });
    }
    const count = drafts.length;
    clearComposer();
    // The empty state's Add button is about to disappear with the first
    // download, so focus returns to the app bar's Add download instead.
    dialogOpener = null;
    addDialog.close();
    renderedQueueSignature = "";
    setQueueSummary(count === 1 ? "Added 1 download to the queue." : `Added ${count} downloads to the queue.`);
    await refreshQueue();
  } catch (error) {
    showError(formError, error);
  } finally {
    setComposerAvailability(true);
  }
});

/* Queue controls ---------------------------------------------------------- */

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
    await invoke("cancel_download", { jobId: current.jobId });
    await refreshQueue();
  } catch (error) {
    showError(formError, error);
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
      await invoke(mode === "pause" ? "pause_download" : "resume_download", { jobId: job.jobId });
      moved += 1;
    } catch {
      // Already finished, or no longer eligible. The next refresh shows why.
    }
  }
  button.disabled = false;
  await refreshQueue();
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
      void openDetails(card.dataset.jobId!, null);
    }
    return;
  }
  if (button.dataset.action === "details") {
    void openDetails(button.dataset.jobId!, button);
    return;
  }
  const jobId = button.dataset.jobId!;
  const job = jobs.find((candidate) => candidate.jobId === jobId);
  if (!job) return;
  button.disabled = true;
  try {
    switch (button.dataset.action) {
      case "cancel":
        await invoke("cancel_download", { jobId });
        break;
      case "pause":
        await invoke("pause_download", { jobId });
        break;
      case "approve":
        await invoke("approve_download", { jobId });
        announce(`${filename(job.destination) || "The download"} was approved.`);
        break;
      case "deny":
        await invoke("deny_download", { jobId });
        announce(`${filename(job.destination) || "The download"} was denied.`);
        break;
      case "resume":
        await invoke("resume_download", { jobId });
        break;
      case "start-now":
        await invoke("start_now", { jobId });
        break;
      case "retry":
        await invoke("retry_download", { jobId, url: null, destination: null });
        break;
      case "choose-new-path": {
        const selected = await save({
          title: "Choose a new destination",
          defaultPath: job.destination ?? suggestedFilename(job.source),
        });
        if (selected) await invoke("retry_download", { jobId, url: null, destination: selected });
        break;
      }
      case "edit-link":
        beginEdit(job);
        return;
      case "edit-checksum":
        beginChecksumEdit(job);
        return;
      case "recapture":
        setQueueSummary("Open the source in your browser and choose Send link to Fetchpath again.");
        return;
      case "configure-media":
        await showSettings("integrations");
        return;
      case "open-folder":
        await invoke("reveal_download", { jobId });
        setQueueSummary("Opened the folder in File Explorer.");
        return;
      case "copy-path":
        if (job.destination) await navigator.clipboard.writeText(job.destination);
        setQueueSummary("Destination copied.");
        break;
      case "remove":
        if (
          job.state === "completed" &&
          settingsView?.settings.confirmRemoveCompleted &&
          !window.confirm(
            `Remove ${filename(job.destination) || "this download"} from the list?\n\n` +
              "The downloaded file stays on your computer.",
          )
        ) {
          return;
        }
        await invoke("remove_download", { jobId });
        break;
    }
    await refreshQueue();
  } catch (error) {
    showError(formError, error);
  } finally {
    button.disabled = false;
    if (button.isConnected) button.focus({ preventScroll: true });
  }
});

document.addEventListener("keydown", (event) => {
  // While Settings or help is modal it owns every key. Escape in Add download
  // is the dialog's own cancel, which closes it and keeps the draft.
  if (shortcutsDialog.open || settingsDialog.open || detailsDialog.open) return;
  const key = event.key.toLowerCase();
  if (event.ctrlKey && (key === "l" || key === "n")) {
    event.preventDefault();
    openComposer();
  } else if (event.ctrlKey && event.key === ",") {
    event.preventDefault();
    void showSettings();
  } else if (event.ctrlKey && key === "f" && !addDialog.open) {
    event.preventDefault();
    queueSearch.focus();
    queueSearch.select();
  }
});

/* Rendering --------------------------------------------------------------- */

function buildDrafts(): JobDraft[] {
  const urls = parseUrls();
  const baseDestination = destinationInput.value.trim();
  // An empty destination is only valid for a torrent: the engine chooses.
  if (!urls.length || (!baseDestination && downloadKind() !== "torrent")) return [];
  const notBeforeMs = scheduleInput.value ? new Date(scheduleInput.value).getTime() : null;
  const used = new Set<string>();
  return urls.map((url, index) => {
    let destination = index === 0 ? baseDestination : siblingDestination(baseDestination, suggestedFilename(url));
    destination = uniqueDestination(destination, used);
    used.add(destination.toLocaleLowerCase());
    const checksum = downloadKind() === "file" ? checksumInput.value.trim() || null : null;
    return { url, destination, notBeforeMs, checksum };
  });
}

function renderPreview(): void {
  const drafts = buildDrafts();
  batchPreview.hidden = drafts.length === 0;
  previewList.replaceChildren();
  if (!drafts.length) return;
  previewCount.textContent = drafts.length === 1 ? "1 item" : `${drafts.length} items`;
  for (const draft of drafts) {
    const item = document.createElement("li");
    const name = document.createElement("strong");
    name.textContent = draft.destination ? filename(draft.destination) : "Default download folder";
    const source = document.createElement("span");
    source.textContent = redactedSource(draft.url);
    item.append(name, source);
    previewList.append(item);
  }
  startButton.textContent = editingJobId
    ? "Save and retry"
    : drafts.length === 1
      ? "Add to queue"
      : `Add ${drafts.length} to queue`;
}

async function refreshQueue(): Promise<void> {
  if (refreshRunning) {
    refreshAgain = true;
    return;
  }
  refreshRunning = true;
  try {
    jobs = await invoke<JobSnapshot[]>("list_downloads");
    recordSpeeds(jobs);
    renderQueue();
    if (detailsDialog.open) await refreshDetails();
    if (settingsView?.settings.powerMode) await refreshStats();
    // A video page sent from the browser opens here so its quality can be
    // chosen; the link analysis picks a sensible one on its own.
    // Taking a review consumes it, so one that arrives while the person is busy
    // elsewhere waits here rather than being dropped.
    pendingReviews.push(...(await invoke<string[]>("take_link_reviews")));
    const busy = settingsDialog.open || shortcutsDialog.open || (addDialog.open && urlInput.value.trim() !== "");
    if (pendingReviews.length && !busy) {
      if (detailsDialog.open) detailsDialog.close();
      openComposer(pendingReviews.splice(0).join("\n"));
      announce("A link from your browser is ready to add.");
    }
  } catch (error) {
    showError(formError, typeof error === "string" ? error : "Could not refresh the queue.");
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

interface EngineConnection {
  connected: boolean;
  /** Stopped on purpose, or would not start: the window waits for the person. */
  stopped?: boolean;
  message?: string;
  /** Connected to a queue saved by a newer Fetchpath: shown, never changed. */
  readOnly?: string;
}

const ENGINE_LOST =
  "Fetchpath's engine stopped. Reconnecting… Downloads continue from their saved progress once it is back.";
const ENGINE_STOPPED =
  "Fetchpath's engine is stopped, so downloads are waiting. Start it to continue where they left off.";

/** Reads the host's current view rather than trusting event order: two
 *  reports a moment apart may arrive in either order. */
async function syncEngineState(): Promise<void> {
  showEngineState(await invoke<EngineConnection>("engine_connection"));
}

function revealEngineNotice(text: string, canStart: boolean): void {
  engineStart.hidden = !canStart;
  if (!engineStatus.hidden && engineStatusText.textContent === text) return;
  engineStatusText.textContent = text;
  engineStatus.hidden = false;
  announceProblem(text);
}

function showEngineState(state: EngineConnection): void {
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
      announce("Fetchpath is running again.");
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
    const now = await invoke<EngineConnection>("engine_connection");
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
    renderStats(await invoke<QueueStats>("queue_stats"));
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

function renderQueue(): void {
  const focusedAction = document.activeElement instanceof HTMLButtonElement && jobList.contains(document.activeElement)
    ? { jobId: document.activeElement.dataset.jobId, action: document.activeElement.dataset.action }
    : null;
  const active = jobs.filter((job) => isActive(job.state)).length;
  activeCount.textContent = String(active);
  activeStatText.textContent =
    active === 0 ? "No downloads are active." : active === 1 ? "1 download active now." : `${active} downloads active now.`;

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
  article.setAttribute("aria-label", `${name}, ${stateLabel(job.state)}`);

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
  status.dataset.state = job.state;
  status.textContent = stateLabel(job.state);
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

  const progress = document.createElement("progress");
  progress.setAttribute("aria-label", `Progress for ${name}`);
  if (primary) progress.id = "job-progress";
  applyProgress(progress, job);
  article.append(progress);

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

  if (settingsView?.settings.powerMode) article.append(diagnostics(job));

  // Measurements and actions share the last line, so a row stays compact.
  const footer = document.createElement("div");
  footer.className = "job-footer";
  const actions = document.createElement("div");
  actions.className = "job-actions";
  for (const action of actionsFor(job)) actions.append(actionButton(job, action));
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

/* Details window ------------------------------------------------------------
   Everything here is drawn from engine-reported values: the smoothed rate the
   queue already shows, bytes written, and the ranges in flight. Nothing is
   interpolated between samples, and a stopped download shows no speed. */

function recordSpeeds(snapshot: JobSnapshot[]): void {
  const now = Date.now();
  const present = new Set<string>();
  for (const job of snapshot) {
    present.add(job.jobId);
    if (job.state !== "running") continue;
    const samples = speedHistory.get(job.jobId) ?? [];
    samples.push({ at: now, bytesPerSecond: job.bytesPerSecond ?? 0 });
    while (samples.length && samples[0].at < now - SPEED_WINDOW_MS) samples.shift();
    speedHistory.set(job.jobId, samples);
  }
  for (const jobId of speedHistory.keys()) if (!present.has(jobId)) speedHistory.delete(jobId);
}

async function openDetails(jobId: string, opener: HTMLElement | null): Promise<void> {
  detailsJobId = jobId;
  if (!(await refreshDetails())) return;
  // A click on the card leaves focus on the body; return it to the row instead.
  if (opener === null) jobList.querySelector<HTMLElement>(`button[data-action="details"][data-job-id="${CSS.escape(jobId)}"]`)?.focus();
  openDialog(detailsDialog, detailsClose);
}

detailsClose.addEventListener("click", () => detailsDialog.close());
detailsDialog.addEventListener("close", () => {
  const jobId = detailsJobId;
  detailsJobId = null;
  const button = jobId
    ? jobList.querySelector<HTMLElement>(`button[data-action="details"][data-job-id="${CSS.escape(jobId)}"]`)
    : null;
  restoreDialogFocus(button ?? queueTitle);
});

/** Returns false when the download is gone, having closed the window. */
async function refreshDetails(): Promise<boolean> {
  if (!detailsJobId) return false;
  let details: JobDetails;
  try {
    details = await invoke<JobDetails>("download_details", { jobId: detailsJobId });
  } catch {
    if (detailsDialog.open) detailsDialog.close();
    return false;
  }
  renderDetails(details);
  return true;
}

function renderDetails({ job, segments }: JobDetails): void {
  const name = filename(job.destination) || "Download";
  detailsTitle.textContent = name;
  detailsSource.textContent = job.source;
  detailsSource.title = job.source;
  detailsStatus.dataset.state = job.state;
  detailsStatus.textContent = stateLabel(job.state);

  const running = job.state === "running";
  const known = job.totalBytes && job.totalBytes > 0 ? job.totalBytes : null;
  const inFlight = segments.reduce((sum, segment) => sum + segment.received, 0);
  const figures: Array<[string, string]> = [
    ["Speed", running && job.bytesPerSecond ? `${formatBytes(job.bytesPerSecond)}/s` : "—"],
    [
      "Received",
      known
        ? `${formatBytes(job.bytesReceived)} of ${formatBytes(known)}`
        : `${formatBytes(job.bytesReceived)}`,
    ],
    [
      "Progress",
      job.state === "completed" ? "100%" : known ? `${Math.floor(Math.min(job.bytesReceived / known, 1) * 100)}%` : "—",
    ],
    ["Time left", running && job.etaSeconds != null ? formatDurationLong(job.etaSeconds) : "—"],
    ["Connections", running ? String(Math.max(segments.length, 1)) : "—"],
  ];
  detailsFigures.replaceChildren(...figures.map(([term, value]) => figureGroup(term, value)));

  renderSpeedGraph(job);
  renderFileMap(job, segments, known, inFlight);
  renderDetailsInfo(job);
}

function figureGroup(term: string, value: string): HTMLElement {
  const group = document.createElement("div");
  const dt = document.createElement("dt");
  dt.textContent = term;
  const dd = document.createElement("dd");
  dd.textContent = value;
  group.append(dt, dd);
  return group;
}

const SVG_NS = "http://www.w3.org/2000/svg";

function svgElement<K extends keyof SVGElementTagNameMap>(
  tag: K,
  attributes: Record<string, string | number>,
): SVGElementTagNameMap[K] {
  const element = document.createElementNS(SVG_NS, tag);
  for (const [key, value] of Object.entries(attributes)) element.setAttribute(key, String(value));
  return element;
}

/** Replaces everything in an SVG except its accessible `<title>`. */
function redraw(svg: SVGSVGElement, title: SVGTitleElement, ...children: SVGElement[]): void {
  svg.replaceChildren(title, ...children);
}

function renderSpeedGraph(job: JobSnapshot): void {
  const width = 600;
  const height = 140;
  const samples = speedHistory.get(job.jobId) ?? [];
  const now = Date.now();
  const peak = samples.reduce((max, sample) => Math.max(max, sample.bytesPerSecond), 0);
  const grid = [0.25, 0.5, 0.75].map((fraction) =>
    svgElement("line", { x1: 0, x2: width, y1: height * fraction, y2: height * fraction, class: "graph-grid" }),
  );
  if (samples.length < 2 || peak === 0) {
    redraw(detailsGraph, detailsGraphLabel, ...grid);
    detailsGraphLabel.textContent = "Speed over the last minute: no transfer measured yet";
    detailsGraphNote.textContent =
      job.state === "running"
        ? "Measuring…"
        : "Speed is drawn while a download is running in this window's session.";
    return;
  }
  // Headroom above the peak so the line never touches the top edge.
  const scale = peak * 1.15;
  const x = (at: number) => ((at - (now - SPEED_WINDOW_MS)) / SPEED_WINDOW_MS) * width;
  const y = (rate: number) => height - (rate / scale) * height;
  const points = samples.map((sample) => `${x(sample.at).toFixed(1)},${y(sample.bytesPerSecond).toFixed(1)}`);
  const first = x(samples[0].at).toFixed(1);
  const last = x(samples[samples.length - 1].at).toFixed(1);
  const area = svgElement("polygon", {
    points: `${first},${height} ${points.join(" ")} ${last},${height}`,
    class: "graph-area",
  });
  const line = svgElement("polyline", { points: points.join(" "), class: "graph-line" });
  redraw(detailsGraph, detailsGraphLabel, ...grid, area, line);
  const average = samples.reduce((sum, sample) => sum + sample.bytesPerSecond, 0) / samples.length;
  const summary = `Peak ${formatBytes(peak)}/s · average ${formatBytes(Math.round(average))}/s over the last ${formatDurationLong(
    Math.max(1, Math.round((now - samples[0].at) / 1000)),
  )}`;
  detailsGraphLabel.textContent = `Speed over the last minute. ${summary}.`;
  detailsGraphNote.textContent = summary;
}

function renderFileMap(job: JobSnapshot, segments: SegmentView[], known: number | null, inFlight: number): void {
  const width = 600;
  const height = 18;
  const track = svgElement("rect", { x: 0, y: 0, width, height, class: "map-pending" });
  if (!known) {
    // With no stated size there is no whole to draw parts of.
    redraw(detailsMap, detailsMapLabel, track);
    detailsMapLabel.textContent = "The source did not state the file's size, so its parts cannot be drawn.";
  } else {
    const at = (byte: number) => (Math.min(byte, known) / known) * width;
    const written = job.state === "completed" ? known : job.bytesReceived;
    const parts: SVGElement[] = [track, svgElement("rect", { x: 0, y: 0, width: at(written), height, class: "map-written" })];
    for (const segment of segments) {
      // At least one pixel wide, so a range in a large file is still visible.
      const left = at(segment.start);
      const span = Math.max(at(segment.end + 1) - left, 1);
      parts.push(svgElement("rect", { x: left, y: 0, width: span, height, class: "map-requested" }));
      const size = segment.end - segment.start + 1;
      const filled = (Math.min(segment.received, size) / size) * span;
      parts.push(svgElement("rect", { x: left, y: 0, width: filled, height, class: "map-inflight" }));
    }
    redraw(detailsMap, detailsMapLabel, ...parts);
    detailsMapLabel.textContent =
      `${formatBytes(written)} of ${formatBytes(known)} written` +
      (segments.length ? `, ${formatBytes(inFlight)} arriving on ${segments.length} connection${segments.length === 1 ? "" : "s"}` : "");
  }

  detailsSegments.replaceChildren(
    ...segments.map((segment, index) => {
      const size = segment.end - segment.start + 1;
      const item = document.createElement("li");
      const label = document.createElement("span");
      label.className = "segment-label";
      label.textContent = `Connection ${index + 1} · ${formatBytes(segment.start)}–${formatBytes(segment.end + 1)}`;
      const bar = document.createElement("progress");
      bar.max = size;
      bar.value = Math.min(segment.received, size);
      bar.setAttribute("aria-label", `Connection ${index + 1}`);
      bar.setAttribute("aria-valuetext", `${formatBytes(segment.received)} of ${formatBytes(size)}`);
      const amount = document.createElement("span");
      amount.className = "segment-amount";
      amount.textContent = `${Math.floor((Math.min(segment.received, size) / size) * 100)}%`;
      item.append(label, bar, amount);
      return item;
    }),
  );
  detailsSegmentsNote.textContent =
    job.state !== "running"
      ? ""
      : job.kind === "media" || job.kind === "torrent"
        ? "This download is fetched by its isolated helper, which does not report individual connections."
        : segments.length
          ? "Pieces are fetched side by side and written in order once each group has arrived."
          : "One connection. Fetchpath splits a download into pieces only when the server supports it and the file is large enough.";
}

function renderDetailsInfo(job: JobSnapshot): void {
  const rows: Array<[string, string]> = [
    ["Saved to", job.destination ?? "Not chosen yet"],
    ["Total size", job.totalBytes === null ? "Not stated by the source" : `${formatBytes(job.totalBytes)} (${job.totalBytes.toLocaleString()} bytes)`],
    ["Kind", job.kind === "torrent" ? "Torrent" : job.kind === "media" ? `Media${job.qualityLabel ? ` · ${job.qualityLabel}` : ""}` : "File"],
    ["Added", new Date(job.createdAtMs).toLocaleString()],
  ];
  if (job.finishedAtMs) {
    rows.push(["Finished", new Date(job.finishedAtMs).toLocaleString()]);
    // Includes any time spent waiting in the queue, so no average speed is
    // derived from it.
    const elapsed = Math.round((job.finishedAtMs - job.createdAtMs) / 1000);
    if (elapsed > 0) rows.push(["Added to finished", formatDurationLong(elapsed)]);
  }
  if (job.attempt > 0) rows.push(["Automatic retries", String(job.attempt)]);
  if (job.observedSha256) rows.push(["SHA-256", job.observedSha256]);
  if (job.error) rows.push(["Problem", friendlyError(job)]);
  detailsInfo.replaceChildren(
    ...rows.flatMap(([term, value]) => {
      const dt = document.createElement("dt");
      dt.textContent = term;
      const dd = document.createElement("dd");
      if (term === "SHA-256") {
        const code = document.createElement("code");
        code.textContent = value;
        dd.append(code);
      } else dd.textContent = value;
      return [dt, dd];
    }),
  );
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

function actionsFor(job: JobSnapshot): Array<{ action: string; label: string; danger?: boolean }> {
  const actions: Array<{ action: string; label: string; danger?: boolean }> = [];
  if (job.state === "awaiting_approval") {
    actions.push({ action: "approve", label: "Approve" });
    actions.push({ action: "deny", label: "Deny", danger: true });
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
    if (job.action === "choose_new_path") actions.push({ action: "choose-new-path", label: "Choose new path" });
    else if (job.action === "edit_link") actions.push({ action: "edit-link", label: "Edit link" });
    else if (job.action === "recapture") actions.push({ action: "recapture", label: "Send again from browser" });
    else if (job.action === "refresh_source") actions.push({ action: "edit-link", label: "Refresh source" });
    else if (job.action === "configure_media_tools") actions.push({ action: "configure-media", label: "Set up media tools" });
    else if (job.action === "check_checksum") {
      actions.push({ action: "edit-checksum", label: "Edit checksum" });
      actions.push({ action: "retry", label: "Retry" });
    } else actions.push({ action: "retry", label: "Retry" });
  }
  if (job.state === "completed") {
    actions.push({ action: "open-folder", label: "Open folder" });
    actions.push({ action: "copy-path", label: "Copy path" });
  }
  actions.push({ action: "details", label: "Details" });
  if (["completed", "cancelled", "failed"].includes(job.state)) actions.push({ action: "remove", label: "Remove" });
  return actions;
}

function actionButton(job: JobSnapshot, spec: { action: string; label: string; danger?: boolean }): HTMLButtonElement {
  const button = document.createElement("button");
  button.type = "button";
  button.className = spec.danger ? "secondary danger" : "secondary";
  button.dataset.action = spec.action;
  button.dataset.jobId = job.jobId;
  button.textContent = spec.label;
  // "Retry" repeated on five cards is five identical accessible names in the
  // automation tree, so every row action names the download it acts on.
  button.setAttribute("aria-label", `${spec.label}: ${filename(job.destination) || "download"}`);
  if (job.state === "cancelling" && spec.action === "cancel") button.disabled = true;
  return button;
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
    announceProblem(`${approvalText(asking[0])} Approve or deny it in the queue.`);
  } else if (asking.length > 1) {
    announceProblem(`${asking.length} agent requests wait for your approval. Approve or deny them in the queue.`);
  }
  for (const job of jobs) {
    next.set(job.jobId, job.state);
    const previous = observedStates.get(job.jobId);
    if (previous === job.state || previous === undefined) continue;
    const name = filename(job.destination) || "download";
    if (job.state === "completed") {
      announce(`${name} finished downloading.`);
    } else if (job.state === "failed" || job.state === "needs_source") {
      announceProblem(`${name} needs attention. ${friendlyError(job)}`);
    } else if (job.state === "cancelled") {
      announce(`${name} was cancelled.`);
    } else if (job.state === "paused") {
      announce(`${name} is paused at ${formatBytes(job.bytesReceived)}.`);
    }
  }
  observedStates = next;
}

function beginEdit(job: JobSnapshot): void {
  editingJobId = job.jobId;
  const kind = form.querySelector<HTMLInputElement>(`input[name="download-kind"][value="${job.kind}"]`);
  if (kind) kind.checked = true;
  mediaOptions.hidden = job.kind !== "media";
  torrentOptions.hidden = true;
  checksumField.hidden = job.kind !== "file";
  if (job.kind === "media") {
    mediaTitle.textContent = `${job.qualityLabel ?? "Selected quality"} will be revalidated before retrying.`;
  }
  urlInput.value = "";
  destinationInput.value = job.destination ?? "";
  destinationIsSuggested = false;
  scheduleInput.value = "";
  // Kept, so refreshing a link never silently drops its check.
  checksumInput.value = job.expectedSha256 ?? "";
  startButton.textContent = "Save and retry";
  clearError(formError);
  showNotice("Paste a refreshed address. Private query values are never restored from history.");
  openComposer();
}

function beginChecksumEdit(job: JobSnapshot): void {
  beginEdit(job);
  editingChecksum = true;
  advancedOptions.open = true;
  showNotice("Correct the checksum, then choose Save and retry. Leave the address empty to keep the current link.");
  checksumInput.focus();
  checksumInput.select();
}

function clearComposer(): void {
  editingJobId = null;
  editingChecksum = false;
  form.reset();
  destinationIsSuggested = true;
  kindChosenByPerson = false;
  analyzedUrl = "";
  advisedUrl = "";
  ruleFolder = null;
  ruleNote.hidden = true;
  ruleNote.textContent = "";
  repositoryUrl = "";
  showRepository(null);
  window.clearTimeout(analysisTimer);
  mediaInspection = null;
  inspectedMediaUrl = "";
  mediaOptions.hidden = true;
  torrentOptions.hidden = true;
  checksumField.hidden = false;
  mediaQuality.replaceChildren(new Option("Inspect a link first", ""));
  mediaQuality.disabled = true;
  mediaTitle.textContent = "";
  urlInput.rows = 3;
  urlInput.placeholder = "https://example.com/archive.zip";
  destinationInput.placeholder = "Choose a destination file";
  clearError(formError);
  clearNotice();
  renderPreview();
  startButton.textContent = "Add to queue";
}

function setComposerAvailability(available: boolean, label?: string): void {
  // Disabling the control that was activated would drop focus to the document
  // body, so the focused element is remembered and restored around the work.
  if (!available && form.contains(document.activeElement) && document.activeElement instanceof HTMLElement) {
    composerFocus = document.activeElement;
  }
  for (const control of form.querySelectorAll<
    HTMLInputElement | HTMLTextAreaElement | HTMLButtonElement | HTMLSelectElement
  >("input, textarea, button, select")) {
    control.disabled = !available;
  }
  if (available && downloadKind() === "media" && !mediaInspection) mediaQuality.disabled = true;
  if (label) startButton.textContent = label;
  else renderPreview();
  if (!available) return;
  const restore = composerFocus;
  composerFocus = null;
  if (restore && form.contains(restore) && !(restore as HTMLButtonElement).disabled) restore.focus({ preventScroll: true });
  else if (restore) startButton.focus({ preventScroll: true });
}

/* Helpers ----------------------------------------------------------------- */

function parseUrls(): string[] {
  return urlInput.value
    .split(/\r?\n/)
    .map((value) => value.trim())
    .filter(Boolean);
}

function joinPath(directory: string | null, name: string): string {
  if (!directory) return name;
  const separator = directory.includes("/") && !directory.includes("\\") ? "/" : "\\";
  return directory.endsWith(separator) ? `${directory}${name}` : `${directory}${separator}${name}`;
}

function siblingDestination(base: string, name: string): string {
  const separatorIndex = Math.max(base.lastIndexOf("\\"), base.lastIndexOf("/"));
  return separatorIndex < 0 ? name : `${base.slice(0, separatorIndex + 1)}${name}`;
}

function uniqueDestination(destination: string, used: Set<string>): string {
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

function matchesFilter(job: JobSnapshot, filter: QueueFilter): boolean {
  if (filter === "all") return true;
  if (filter === "active") return isActive(job.state);
  if (filter === "paused") return job.state === "paused";
  if (filter === "scheduled") return job.state === "scheduled" || job.state === "queued";
  if (filter === "completed") return job.state === "completed";
  return job.state === "failed" || job.state === "needs_source" || job.state === "awaiting_approval";
}

/** Who asks and why, in the words the terminal's card uses. */
function approvalText(job: JobSnapshot): string {
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

function matchesSearch(job: JobSnapshot, query: string): boolean {
  if (!query) return true;
  return [job.source, job.destination, filename(job.destination), job.error, job.qualityLabel, job.agent]
    .filter((value): value is string => Boolean(value))
    .some((value) => value.toLocaleLowerCase().includes(query));
}

function isActive(state: JobState): boolean {
  return state === "running" || state === "cancelling";
}

function stateLabel(state: JobState): string {
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

function friendlyError(job: JobSnapshot): string {
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
function checksumMismatch(job: JobSnapshot): { expected: string; received: string } | null {
  if (job.state !== "failed" || !job.error?.includes("checksum_mismatch")) return null;
  const expected = /expected sha256 ([0-9a-f]{64})/.exec(job.error)?.[1];
  const received = /(?:received|holds) ([0-9a-f]{64})/.exec(job.error)?.[1];
  return expected && received ? { expected, received } : null;
}

function formatBytes(bytes: number): string {
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
function formatDurationLong(seconds: number): string {
  if (seconds < 60) return `${Math.max(seconds, 1)} sec`;
  if (seconds < 3600) {
    const minutes = Math.round(seconds / 60);
    return minutes === 1 ? "1 min" : `${minutes} min`;
  }
  const hours = Math.floor(seconds / 3600);
  const minutes = Math.round((seconds % 3600) / 60);
  return minutes ? `${hours} hr ${minutes} min` : `${hours} hr`;
}

function suggestedFilename(value: string): string {
  try {
    const segments = new URL(value).pathname.split("/").filter(Boolean);
    const segment = segments[segments.length - 1];
    return sanitizeFilename(segment ? decodeURIComponent(segment) : "download.bin");
  } catch {
    return "download.bin";
  }
}

function downloadKind(): "file" | "media" | "torrent" {
  const value = form.querySelector<HTMLInputElement>('input[name="download-kind"]:checked')?.value;
  return value === "media" || value === "torrent" ? value : "file";
}

function selectedMediaVariant(): MediaVariant | null {
  return mediaInspection?.variants.find((variant) => variant.id === mediaQuality.value) ?? null;
}

function setMediaDestination(variant: MediaVariant, replaceExtension = false): void {
  const current = destinationInput.value.trim();
  const title = sanitizeFilename(mediaInspection?.title ?? "media");
  // Fetchpath's own suggestion (for a YouTube link, "watch") gives way to the
  // video's title; a name the person typed or picked is kept.
  if (!current || destinationIsSuggested) {
    destinationInput.value = joinPath(ruleFolder ?? defaultDestinationDir, `${title}.${variant.extension}`);
    return;
  }
  if (replaceExtension) {
    const separator = Math.max(current.lastIndexOf("/"), current.lastIndexOf("\\"));
    const dot = current.lastIndexOf(".");
    destinationInput.value = `${dot > separator ? current.slice(0, dot) : current}.${variant.extension}`;
  }
}

function formatDuration(seconds: number): string {
  const rounded = Math.max(0, Math.round(seconds));
  const minutes = Math.floor(rounded / 60);
  const remainder = rounded % 60;
  return `${minutes}:${String(remainder).padStart(2, "0")}`;
}

function sanitizeFilename(value: string): string {
  const safe = value.replace(/[<>:"/\\|?*\u0000-\u001f]/g, "_").replace(/[. ]+$/g, "");
  return safe || "download.bin";
}

function filename(path: string | null): string {
  const segments = path?.split(/[\\/]/).filter(Boolean) ?? [];
  return segments[segments.length - 1] ?? "";
}

function redactedSource(url: string): string {
  const index = url.search(/[?#]/);
  return index < 0 ? url : `${url.slice(0, index)}?…`;
}

function showError(element: HTMLElement, error: unknown): void {
  const message =
    error instanceof Error ? error.message : typeof error === "string" ? error : "Something went wrong. Please try again.";
  element.textContent = message;
  element.hidden = false;
  if (element === settingsError) {
    settingsStatus.hidden = true;
    announceProblem(message);
    return;
  }
  if (element !== formError) return;
  announceProblem(message);
  associateError(message);
}

function clearError(element: HTMLElement): void {
  element.textContent = "";
  element.hidden = true;
  if (element === settingsError) {
    settingsStatus.hidden = true;
    return;
  }
  if (element !== formError) return;
  liveAlert.textContent = "";
  announcedAlert = "";
  associateError(null);
}

/**
 * Points the failing control at the message that describes it, so the field and
 * its error are read together instead of the error living alone in the page.
 */
function associateError(message: string | null): void {
  const culprit = !message
    ? null
    : /destination|folder|file ?name|reserved|drive|dot or a space|Windows cannot use|same folder|already exists/i.test(
          message,
        )
      ? destinationInput
      : /address|link|url|http|password|user name|quality|media/i.test(message)
        ? urlInput
        : null;
  for (const field of [urlInput, destinationInput] as const) {
    const base = field === urlInput ? "url-hint" : "destination-hint";
    if (field === culprit) {
      field.setAttribute("aria-invalid", "true");
      field.setAttribute("aria-describedby", `form-error ${base}`);
    } else {
      field.removeAttribute("aria-invalid");
      field.setAttribute("aria-describedby", base);
    }
  }
}

function showNotice(message: string): void {
  formNotice.textContent = message;
  formNotice.hidden = false;
  announce(message);
}

function clearNotice(): void {
  formNotice.textContent = "";
  formNotice.hidden = true;
}

function required<T extends Element>(id: string): T {
  const element = document.getElementById(id);
  if (!element) throw new Error(`Missing required element: ${id}`);
  return element as Element as T;
}

/* Start ------------------------------------------------------------------- */

async function start(): Promise<void> {
  // Settings decide the theme, whether the statistics panel exists and whether
  // the welcome card shows, so they are read before the first paint of the
  // queue rather than after it.
  await loadSettings();
  try {
    defaultDestinationDir = await invoke<string | null>("default_destination_dir");
  } catch {
    defaultDestinationDir = systemDownloadDir;
  }
  void refreshToolsStatus();
  renderPreview();
  await listen("fetchpath://queue", () => void refreshQueue());
  await listen("fetchpath://engine", () => void syncEngineState());
  // The engine may have been lost before this page was listening.
  await syncEngineState();
  await refreshQueue();
  // Changes arrive as events; this slower pass catches what has no event of
  // its own, such as a schedule coming due or a link sent from the browser.
  refreshTimer = window.setInterval(() => void refreshQueue(), 1000);
}

void start();
window.addEventListener("beforeunload", () => window.clearInterval(refreshTimer));
