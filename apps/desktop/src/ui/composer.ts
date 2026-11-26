import { invoke } from "@tauri-apps/api/core";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { open, save } from "@tauri-apps/plugin-dialog";
import type { EngineApi } from "../engine/api";
import type { JobDraft, JobSnapshot } from "../engine/types";
import { type DialogManager, errorMessage } from "./dialogs";
import { required } from "./dom";
import {
  filename,
  formatBytes,
  formatDuration,
  joinPath,
  redactedSource,
  sanitizeFilename,
  siblingDestination,
  suggestedFilename,
  uniqueDestination,
} from "./format";
import type { RuleAdvice } from "./rules";

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

interface PageImageView {
  url: string;
  filename: string;
  title: string | null;
  contentType: string | null;
}

export interface ComposerContext {
  engine: EngineApi;
  dialogs: DialogManager;
  addOpenButton: HTMLButtonElement;
  defaultDestinationDir(): string | null;
  mediaToolsReady(): boolean;
  hasToolsStatus(): boolean;
  refreshToolsStatus(): Promise<void>;
  hasBlockingModalOpen(): boolean;
  onSubmitted(count: number): Promise<void>;
  onDialogClose(): void;
}

export interface Composer {
  readonly dialog: HTMLDialogElement;
  readonly addOpenButton: HTMLButtonElement;
  hasDraft(): boolean;
  open(prefill?: string): void;
  beginEdit(job: JobSnapshot): void;
  beginChecksumEdit(job: JobSnapshot): void;
  showNotice(message: string): void;
  renderPreview(): void;
  reanalyzeIfReady(wasReady: boolean, ready: boolean): void;
}

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

function isPhotoPageLink(value: string): boolean {
  try {
    const url = new URL(value);
    const host = url.hostname.toLowerCase().replace(/^www\.|^m\./, "");
    return (host === "x.com" || host === "twitter.com") && /\/status\/\d+\/photo\/\d+\/?$/i.test(url.pathname);
  } catch {
    return false;
  }
}

function isRepositoryLink(value: string): boolean {
  return /^(hf:\/\/|https:\/\/(www\.)?huggingface\.co\/)/i.test(value.trim());
}

/** Add Download modal dialog: link classification, media/photo/repo inspection, rule advice and batch preview. */
export function createComposer(ctx: ComposerContext): Composer {
  const { engine, dialogs, addOpenButton } = ctx;
  const addDialog = required<HTMLDialogElement>("add-dialog");
  const addCloseX = required<HTMLButtonElement>("add-close-x");
  const addCancelButton = required<HTMLButtonElement>("add-cancel");
  const emptyAddButton = required<HTMLButtonElement>("empty-add");
  const dropOverlay = required<HTMLElement>("drop-overlay");
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
  const formNotice = required<HTMLParagraphElement>("form-notice");
  const batchPreview = required<HTMLElement>("batch-preview");
  const previewCount = required<HTMLElement>("preview-count");
  const previewList = required<HTMLOListElement>("preview-list");
  const kindChooser = required<HTMLFieldSetElement>("download-kind");
  const mediaOptions = required<HTMLElement>("media-options");
  const torrentOptions = required<HTMLElement>("torrent-options");
  const torrentDiscovery = required<HTMLInputElement>("torrent-discovery");
  const torrentUpload = required<HTMLInputElement>("torrent-upload");
  const destinationLabel = required<HTMLLabelElement>("destination-label");
  const destinationHint = required<HTMLParagraphElement>("destination-hint");
  const torrentPickFile = required<HTMLButtonElement>("torrent-pick-file");
  const inspectMediaButton = required<HTMLButtonElement>("inspect-media");
  const mediaInspectSpinner = required<HTMLElement>("media-inspect-spinner");
  const mediaQuality = required<HTMLSelectElement>("media-quality");
  const mediaTitle = required<HTMLParagraphElement>("media-title");
  const repositoryNote = required<HTMLParagraphElement>("repository-note");

  let editingJobId: string | null = null;
  let editingChecksum = false;
  let mediaInspection: MediaInspection | null = null;
  let inspectedMediaUrl = "";
  let inspectingMedia = false;
  let composerFocus: HTMLElement | null = null;
  let destinationIsSuggested = true;
  let ruleFolder: string | null = null;
  let advisedUrl = "";
  let kindChosenByPerson = false;
  let settingKindAutomatically = false;
  let analysisTimer = 0;
  let analyzedUrl = "";
  let repository: RepositoryView | null = null;
  let repositoryUrl = "";

  function parseUrls(): string[] {
    return urlInput.value
      .split(/\r?\n/)
      .map((value) => value.trim())
      .filter(Boolean);
  }

  function downloadKind(): "file" | "media" | "torrent" {
    const value = form.querySelector<HTMLInputElement>('input[name="download-kind"]:checked')?.value;
    return value === "media" || value === "torrent" ? value : "file";
  }

  function selectedMediaVariant(): MediaVariant | null {
    return mediaInspection?.variants.find((variant) => variant.id === mediaQuality.value) ?? null;
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

  function showFormError(error: unknown): void {
    const message = errorMessage(error);
    formError.textContent = message;
    formError.hidden = false;
    dialogs.announceProblem(message);
    associateError(message);
  }

  function clearFormError(): void {
    formError.textContent = "";
    formError.hidden = true;
    dialogs.clearAlert();
    associateError(null);
  }

  function showNotice(message: string): void {
    formNotice.textContent = message;
    formNotice.hidden = false;
    dialogs.announce(message);
  }

  function clearNotice(): void {
    formNotice.textContent = "";
    formNotice.hidden = true;
  }

  function setMediaDestination(variant: MediaVariant, replaceExtension = false): void {
    const current = destinationInput.value.trim();
    const title = sanitizeFilename(mediaInspection?.title ?? "media");
    // Fetchpath's own suggestion (for a YouTube link, "watch") gives way to the
    // video's title; a name the person typed or picked is kept.
    if (!current || destinationIsSuggested) {
      destinationInput.value = joinPath(ruleFolder ?? ctx.defaultDestinationDir(), `${title}.${variant.extension}`);
      return;
    }
    if (replaceExtension) {
      const separator = Math.max(current.lastIndexOf("/"), current.lastIndexOf("\\"));
      const dot = current.lastIndexOf(".");
      destinationInput.value = `${dot > separator ? current.slice(0, dot) : current}.${variant.extension}`;
    }
  }

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
      return;
    }
    if (!parsed.protocol) return;
    // A repository goes in a folder named after it inside this one.
    if (isRepositoryLink(first)) {
      destinationInput.value = ruleFolder ?? ctx.defaultDestinationDir() ?? "";
      return;
    }
    destinationInput.value = joinPath(ruleFolder ?? ctx.defaultDestinationDir(), suggestedFilename(first));
  }

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
    // Single links already display their filename and URL directly in the form;
    // reserve Batch preview for 2+ links so the common single-link path stays crisp.
    batchPreview.hidden = drafts.length < 2;
    previewList.replaceChildren();
    if (drafts.length >= 2) {
      previewCount.textContent = `${drafts.length} items`;
      for (const draft of drafts) {
        const item = document.createElement("li");
        const name = document.createElement("strong");
        name.textContent = draft.destination ? filename(draft.destination) : "Default download folder";
        const source = document.createElement("span");
        source.textContent = redactedSource(draft.url);
        item.append(name, source);
        previewList.append(item);
      }
    }
    const waitingMedia = inspectingMedia && downloadKind() === "media";
    startButton.disabled = waitingMedia;
    startButton.textContent = waitingMedia
      ? "Inspecting…"
      : editingJobId
        ? "Save and retry"
        : drafts.length > 1
          ? `Add ${drafts.length} to queue`
          : "Add to queue";
  }

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
    if (destinationIsSuggested) destinationInput.value = ruleFolder ?? ctx.defaultDestinationDir() ?? "";
  }

  async function analyzeRepository(url: string): Promise<void> {
    setKind("file");
    repositoryUrl = url;
    showRepository(null, "Looking up this repository…");
    try {
      const view = await invoke<RepositoryView>("inspect_repository", { url });
      if (repositoryUrl === url) showRepository(view);
    } catch (error) {
      if (repositoryUrl === url) showRepository(null, errorMessage(error));
    }
  }

  async function resolvePageImage(url: string): Promise<boolean> {
    try {
      const found = await invoke<PageImageView | null>("inspect_page_image", { url });
      if (!found || parseUrls()[0] !== url) return false;
      kindChosenByPerson = false;
      setKind("file");
      urlInput.value = found.url;
      analyzedUrl = found.url;
      destinationIsSuggested = true;
      destinationInput.value = joinPath(ruleFolder ?? ctx.defaultDestinationDir(), found.filename);
      clearFormError();
      showNotice("Found the photo on this page — ready to add to the queue.");
      renderPreview();
      return true;
    } catch {
      return false;
    }
  }

  async function inspectMedia(url: string, quiet: boolean): Promise<MediaInspection | null> {
    const wasFocused = document.activeElement === inspectMediaButton;
    inspectingMedia = true;
    mediaInspectSpinner.hidden = false;
    inspectMediaButton.disabled = true;
    inspectMediaButton.textContent = "Inspecting…";
    if (!quiet || downloadKind() === "media") mediaTitle.textContent = "Checking the available qualities…";
    renderPreview();
    dialogs.announce("Inspecting the media link.");
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
        showFormError(error);
      } else if (downloadKind() === "media") {
        mediaTitle.textContent = errorMessage(error);
      }
      // The helpers may have gone missing since the last check; re-reading the
      // status turns a repeated failure into the setup path.
      void ctx.refreshToolsStatus();
      return null;
    } finally {
      inspectingMedia = false;
      mediaInspectSpinner.hidden = true;
      inspectMediaButton.disabled = false;
      inspectMediaButton.textContent = "Inspect again";
      renderPreview();
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
    dialogs.announce(`${inspection.title}. ${preferred.label} selected; ${inspection.variants.length} qualities available.`);
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
    if (isPhotoPageLink(url)) {
      setKind("file");
      showNotice("Checking for a photo on this page…");
      if (await resolvePageImage(url)) return;
      clearNotice();
    }
    const ready = ctx.mediaToolsReady();
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
      if (!ready) {
        // Still try resolving a photo page (no yt-dlp needed for images).
        await resolvePageImage(url);
        return;
      }
    } else if (!ready) {
      return;
    }
    const inspection = await inspectMedia(url, true);
    if (!inspection) {
      // When yt-dlp finds no video/audio on a social/media post (for instance a
      // photo post on X), fall back to resolving a public page image.
      if (kind === "media") await resolvePageImage(url);
      return;
    }
    setKind("media");
    applyInspection(url, inspection);
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

  function clearComposer(): void {
    editingJobId = null;
    editingChecksum = false;
    form.reset();
    urlInput.required = true;
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
    inspectingMedia = false;
    mediaInspectSpinner.hidden = true;
    mediaOptions.hidden = true;
    torrentOptions.hidden = true;
    checksumField.hidden = false;
    mediaQuality.replaceChildren(new Option("Inspect a link first", ""));
    mediaQuality.disabled = true;
    mediaTitle.textContent = "";
    urlInput.rows = 3;
    urlInput.placeholder = "https://example.com/archive.zip";
    destinationInput.placeholder = "Choose a destination file";
    destinationInput.required = true;
    destinationLabel.textContent = "Save as";
    destinationHint.textContent = "For a batch, the other files go in the same folder under their own names.";
    clearFormError();
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

  function openComposer(prefill?: string): void {
    if (prefill !== undefined) {
      if (!editingJobId) {
        kindChosenByPerson = false;
        destinationIsSuggested = true;
        urlInput.value = prefill;
      }
      urlInput.dispatchEvent(new Event("input"));
    }
    if (!addDialog.open) dialogs.openDialog(addDialog, urlInput);
    else urlInput.focus();
    urlInput.select();
  }

  function beginEdit(job: JobSnapshot): void {
    editingJobId = job.jobId;
    editingChecksum = false;
    urlInput.required = true;
    const kind = form.querySelector<HTMLInputElement>(`input[name="download-kind"][value="${job.kind}"]`);
    if (kind) kind.checked = true;
    mediaOptions.hidden = job.kind !== "media";
    torrentOptions.hidden = true;
    checksumField.hidden = job.kind !== "file";
    destinationInput.placeholder = "Choose a destination file";
    destinationInput.required = true;
    destinationLabel.textContent = "Save as";
    destinationHint.textContent = "For a batch, the other files go in the same folder under their own names.";
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
    clearFormError();
    showNotice("Paste a refreshed address. Private query values are never restored from history.");
    openComposer();
  }

  function beginChecksumEdit(job: JobSnapshot): void {
    beginEdit(job);
    editingChecksum = true;
    urlInput.required = false;
    advancedOptions.open = true;
    showNotice("Correct the checksum, then choose Save and retry. Leave the address empty to keep the current link.");
    checksumInput.focus();
    checksumInput.select();
  }

  addOpenButton.addEventListener("click", () => openComposer());
  emptyAddButton.addEventListener("click", () => openComposer());
  addCloseX.addEventListener("click", () => addDialog.close());
  addCancelButton.addEventListener("click", () => {
    clearComposer();
    addDialog.close();
  });
  addDialog.addEventListener("close", () => {
    // A half-made correction is not kept: reopening starts a new download.
    if (editingJobId) clearComposer();
    // The draft and its error stay in the dialog for next time, but an alert
    // about a form that is no longer on screen would only confuse.
    dialogs.clearAlert();
    dialogs.restoreDialogFocus(addOpenButton, addDialog);
    ctx.onDialogClose();
  });

  /**
   * Pasting a link anywhere outside a text field opens Add download with it,
   * the way people already move links from a browser.
   */
  document.addEventListener("paste", (event) => {
    const target = event.target as HTMLElement | null;
    if (target?.closest("input, textarea, select, [contenteditable]")) return;
    if (ctx.hasBlockingModalOpen()) return;
    const text = event.clipboardData?.getData("text")?.trim() ?? "";
    const links = text.split(/\s+/).filter((part) => /^(?:https?:\/\/|magnet:\?|hf:\/\/)/i.test(part));
    if (!links.length) return;
    event.preventDefault();
    openComposer(links.join("\n"));
  });

  /**
   * Dragging a URL from a browser or dropping a `.torrent` file onto the window
   * opens Add download pre-filled for review before anything starts.
   */
  let dragDepth = 0;
  function showDropOverlay(show: boolean): void {
    dropOverlay.hidden = !show;
  }

  function extractDroppedLinks(data: DataTransfer | null): string[] {
    if (!data) return [];
    const raw = data.getData("text/uri-list") || data.getData("text/plain") || "";
    return raw
      .split(/\r?\n/)
      .map((line) => line.trim())
      .filter((line) => line && !line.startsWith("#"))
      .filter((line) => /^(?:https?:\/\/|magnet:\?|hf:\/\/)/i.test(line));
  }

  window.addEventListener("dragenter", (event) => {
    if (ctx.hasBlockingModalOpen()) return;
    const types = event.dataTransfer?.types ?? [];
    if (!types.includes("text/uri-list") && !types.includes("text/plain")) return;
    event.preventDefault();
    dragDepth += 1;
    showDropOverlay(true);
  });
  window.addEventListener("dragover", (event) => {
    if (dropOverlay.hidden) return;
    event.preventDefault();
    if (event.dataTransfer) event.dataTransfer.dropEffect = "copy";
  });
  window.addEventListener("dragleave", () => {
    if (dragDepth > 0) dragDepth -= 1;
    if (dragDepth === 0) showDropOverlay(false);
  });
  window.addEventListener("drop", (event) => {
    dragDepth = 0;
    showDropOverlay(false);
    if (ctx.hasBlockingModalOpen()) return;
    const links = extractDroppedLinks(event.dataTransfer);
    if (!links.length) return;
    event.preventDefault();
    openComposer(links.join("\n"));
  });

  void getCurrentWebview()
    .onDragDropEvent((event) => {
      if (ctx.hasBlockingModalOpen()) {
        showDropOverlay(false);
        return;
      }
      if (event.payload.type === "enter" || event.payload.type === "over") {
        const paths = "paths" in event.payload ? event.payload.paths : [];
        if (paths.some((path) => /\.torrent$/i.test(path))) showDropOverlay(true);
      } else if (event.payload.type === "leave") {
        showDropOverlay(false);
      } else if (event.payload.type === "drop") {
        showDropOverlay(false);
        const torrent = event.payload.paths.find((path) => /\.torrent$/i.test(path));
        if (torrent) openComposer(torrent);
      }
    })
    .catch(() => {
      // Running outside the Tauri webview (e.g. Vite preview).
    });

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
    urlInput.placeholder = torrent
      ? "magnet:?xt=urn:btih:…"
      : media
        ? "https://example.com/watch/…"
        : "https://example.com/archive.zip";
    // A torrent needs no Save as: left empty, the engine picks the folder.
    destinationInput.placeholder = torrent
      ? "Optional: a new folder for this torrent"
      : media
        ? "Filled after inspecting the link, or choose a file"
        : "Choose a destination file";
    destinationInput.required = !torrent && !media;
    destinationLabel.textContent = torrent ? "Folder (optional)" : "Save as";
    destinationHint.textContent = torrent
      ? "Left empty, Fetchpath chooses the folder. A folder you name must be new: Fetchpath will not replace one that exists."
      : "For a batch, the other files go in the same folder under their own names.";
    if ((torrent || (media && !mediaInspection)) && destinationIsSuggested) destinationInput.value = "";
    if (media && !ctx.hasToolsStatus()) void ctx.refreshToolsStatus();
    if (!settingKindAutomatically) kindChosenByPerson = true;
    renderPreview();
  });

  torrentPickFile.addEventListener("click", async () => {
    clearFormError();
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
      showFormError(error);
    }
  });

  inspectMediaButton.addEventListener("click", async () => {
    clearFormError();
    const urls = parseUrls();
    if (urls.length !== 1) {
      showFormError("Enter one media address to inspect.");
      return;
    }
    const inspection = await inspectMedia(urls[0], false);
    if (inspection) {
      applyInspection(urls[0], inspection);
    } else if (await resolvePageImage(urls[0])) {
      return;
    }
  });

  urlInput.addEventListener("input", () => {
    if (!urlInput.value.trim()) {
      kindChosenByPerson = false;
      analyzedUrl = "";
    }
    if (downloadKind() === "media" && parseUrls()[0] !== inspectedMediaUrl) {
      mediaInspection = null;
      mediaQuality.replaceChildren(new Option("Inspect this link to continue", ""));
      mediaQuality.disabled = true;
      mediaTitle.textContent = "";
    }
    suggestDestination();
    window.clearTimeout(analysisTimer);
    analysisTimer = window.setTimeout(() => {
      void analyzeLink();
      void adviseRules();
    }, 400);
  });

  mediaQuality.addEventListener("change", () => {
    const variant = selectedMediaVariant();
    if (variant) setMediaDestination(variant, true);
    renderPreview();
  });

  chooseButton.addEventListener("click", async () => {
    clearFormError();
    try {
      const suggested = destinationInput.value || suggestedFilename(parseUrls()[0] ?? "");
      const selected = await save({
        title: downloadKind() === "torrent" ? "Choose a new torrent folder name" : "Save download as",
        defaultPath:
          suggested.includes("\\") || suggested.includes("/")
            ? suggested
            : joinPath(ctx.defaultDestinationDir(), suggested),
      });
      if (selected) {
        destinationInput.value = selected;
        destinationIsSuggested = false;
        renderPreview();
      }
    } catch (error) {
      showFormError(error);
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
    destinationInput.addEventListener(eventName, () => {
      destinationIsSuggested = false;
    });
  }

  form.addEventListener("submit", async (event) => {
    event.preventDefault();
    clearFormError();
    if (!editingJobId && downloadKind() === "media") {
      const urls = parseUrls();
      const variant = selectedMediaVariant();
      if (!urls.length) {
        showFormError("Add at least one download address.");
        return;
      }
      if (!variant || inspectedMediaUrl !== urls[0]) {
        showFormError("Inspect this media link and choose a quality first.");
        return;
      }
    }
    const drafts = buildDrafts();
    if (!editingJobId && !drafts.length) {
      showFormError(parseUrls().length ? "Choose where to save this download." : "Add at least one download address.");
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
        await engine.retryDownload(
          editingJobId,
          urls[0] ?? null,
          destinationInput.value.trim() || null,
          checksumInput.value,
        );
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
        await engine.startBatch(drafts);
      }
      const count = drafts.length;
      clearComposer();
      // The empty state's Add button is about to disappear with the first
      // download, so focus returns to the app bar's Add download instead.
      dialogs.clearDialogOpener(addDialog);
      addDialog.close();
      await ctx.onSubmitted(count);
    } catch (error) {
      showFormError(error);
    } finally {
      setComposerAvailability(true);
    }
  });

  function reanalyzeIfReady(wasReady: boolean, ready: boolean): void {
    // If a link was pasted while startup was still probing yt-dlp/ffmpeg, run
    // analysis now that the helpers are ready.
    if (!wasReady && ready && addDialog.open && !editingJobId && parseUrls().length === 1 && !mediaInspection) {
      analyzedUrl = "";
      void analyzeLink();
    }
  }

  return {
    dialog: addDialog,
    addOpenButton,
    hasDraft: () => addDialog.open && urlInput.value.trim() !== "",
    open: openComposer,
    beginEdit,
    beginChecksumEdit,
    showNotice,
    renderPreview,
    reanalyzeIfReady,
  };
}
