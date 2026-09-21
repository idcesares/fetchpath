import { invoke } from "@tauri-apps/api/core";
import { save } from "@tauri-apps/plugin-dialog";

type JobState =
  | "scheduled"
  | "queued"
  | "running"
  | "cancelling"
  | "completed"
  | "cancelled"
  | "failed"
  | "needs_source";
type QueueFilter = "all" | "active" | "scheduled" | "completed" | "failed";

interface JobSnapshot {
  jobId: string;
  source: string;
  state: JobState;
  bytesReceived: number;
  destination: string | null;
  observedSha256: string | null;
  cleanupPending: boolean;
  error: string | null;
  action: "retry" | "choose_new_path" | "edit_link" | null;
  retryable: boolean;
  createdAtMs: number;
  notBeforeMs: number | null;
  finishedAtMs: number | null;
}

interface JobDraft {
  url: string;
  destination: string;
  notBeforeMs: number | null;
}

const form = required<HTMLFormElement>("download-form");
const urlInput = required<HTMLTextAreaElement>("url");
const destinationInput = required<HTMLInputElement>("destination");
const scheduleInput = required<HTMLInputElement>("schedule");
const clearScheduleButton = required<HTMLButtonElement>("clear-schedule");
const chooseButton = required<HTMLButtonElement>("choose-destination");
const startButton = required<HTMLButtonElement>("start-download");
const formError = required<HTMLParagraphElement>("form-error");
const batchPreview = required<HTMLElement>("batch-preview");
const previewCount = required<HTMLElement>("preview-count");
const previewList = required<HTMLOListElement>("preview-list");
const queueSearch = required<HTMLInputElement>("queue-search");
const queueFilters = required<HTMLElement>("queue-filters");
const queueSummary = required<HTMLParagraphElement>("queue-summary");
const cancelCurrentButton = required<HTMLButtonElement>("cancel-download");
const jobCard = required<HTMLElement>("job-card");
const jobList = required<HTMLDivElement>("job-list");
const activeCount = required<HTMLElement>("active-count");

let jobs: JobSnapshot[] = [];
let selectedFilter: QueueFilter = "all";
let editingJobId: string | null = null;
let refreshRunning = false;
let refreshTimer = 0;

chooseButton.addEventListener("click", async () => {
  clearError(formError);
  try {
    const selected = await save({
      title: "Save download as",
      defaultPath: destinationInput.value || suggestedFilename(parseUrls()[0] ?? ""),
    });
    if (selected) {
      destinationInput.value = selected;
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

form.addEventListener("submit", async (event) => {
  event.preventDefault();
  clearError(formError);
  const drafts = buildDrafts();
  if (!drafts.length) {
    showError(formError, "Add at least one download address.");
    return;
  }
  setComposerAvailability(false, editingJobId ? "Saving…" : "Adding…");
  try {
    if (editingJobId) {
      if (drafts.length !== 1) throw new Error("Edit one recovery item at a time.");
      await invoke<JobSnapshot>("retry_download", {
        jobId: editingJobId,
        url: drafts[0].url,
        destination: drafts[0].destination,
      });
      editingJobId = null;
    } else {
      await invoke<JobSnapshot[]>("start_batch", { drafts });
    }
    const count = drafts.length;
    clearComposer();
    queueSummary.textContent = count === 1 ? "Added 1 download to the queue." : `Added ${count} downloads to the queue.`;
    await refreshQueue();
  } catch (error) {
    showError(formError, error);
  } finally {
    setComposerAvailability(true);
  }
});

queueFilters.addEventListener("click", (event) => {
  const button = (event.target as HTMLElement).closest<HTMLButtonElement>("button[data-filter]");
  if (!button) return;
  selectedFilter = button.dataset.filter as QueueFilter;
  for (const candidate of queueFilters.querySelectorAll<HTMLButtonElement>("button[data-filter]")) {
    const selected = candidate === button;
    candidate.classList.toggle("is-selected", selected);
    candidate.setAttribute("aria-pressed", String(selected));
  }
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
  }
});

jobList.addEventListener("click", async (event) => {
  const button = (event.target as HTMLElement).closest<HTMLButtonElement>("button[data-action][data-job-id]");
  if (!button) return;
  const jobId = button.dataset.jobId!;
  const job = jobs.find((candidate) => candidate.jobId === jobId);
  if (!job) return;
  button.disabled = true;
  try {
    switch (button.dataset.action) {
      case "cancel":
        await invoke("cancel_download", { jobId });
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
      case "copy-path":
        if (job.destination) await navigator.clipboard.writeText(job.destination);
        queueSummary.textContent = "Destination copied.";
        break;
      case "remove":
        await invoke("remove_download", { jobId });
        break;
    }
    await refreshQueue();
  } catch (error) {
    showError(formError, error);
  } finally {
    button.disabled = false;
  }
});

document.addEventListener("keydown", (event) => {
  if (event.ctrlKey && event.key.toLowerCase() === "l") {
    event.preventDefault();
    urlInput.focus();
    urlInput.select();
  } else if (event.key === "Escape" && (editingJobId || urlInput.value || destinationInput.value)) {
    event.preventDefault();
    clearComposer();
    startButton.focus();
  }
});

function buildDrafts(): JobDraft[] {
  const urls = parseUrls();
  const baseDestination = destinationInput.value.trim();
  if (!baseDestination || !urls.length) return [];
  const notBeforeMs = scheduleInput.value ? new Date(scheduleInput.value).getTime() : null;
  const used = new Set<string>();
  return urls.map((url, index) => {
    let destination = index === 0 ? baseDestination : siblingDestination(baseDestination, suggestedFilename(url));
    destination = uniqueDestination(destination, used);
    used.add(destination.toLocaleLowerCase());
    return { url, destination, notBeforeMs };
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
    name.textContent = filename(draft.destination);
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
  if (refreshRunning) return;
  refreshRunning = true;
  try {
    jobs = await invoke<JobSnapshot[]>("list_downloads");
    renderQueue();
  } catch (error) {
    queueSummary.textContent = typeof error === "string" ? error : "Could not refresh the queue.";
  } finally {
    refreshRunning = false;
  }
}

function renderQueue(): void {
  const focusedAction = document.activeElement instanceof HTMLButtonElement && jobList.contains(document.activeElement)
    ? { jobId: document.activeElement.dataset.jobId, action: document.activeElement.dataset.action }
    : null;
  const active = jobs.filter((job) => isActive(job.state)).length;
  activeCount.textContent = String(active);
  const current = jobs.find((job) => ["running", "queued", "scheduled", "cancelling"].includes(job.state));
  cancelCurrentButton.hidden = !current;
  cancelCurrentButton.disabled = current?.state === "cancelling";
  cancelCurrentButton.textContent = current?.state === "cancelling" ? "Cancelling…" : "Cancel current download";
  const query = queueSearch.value.trim().toLocaleLowerCase();
  const visible = jobs.filter((job) => matchesFilter(job, selectedFilter) && matchesSearch(job, query));
  jobCard.hidden = visible.length === 0;
  jobList.replaceChildren();
  if (!jobs.length) {
    queueSummary.textContent = "No downloads yet. Add a link to begin.";
  } else if (!visible.length) {
    queueSummary.textContent = "No downloads match this view.";
  } else {
    queueSummary.textContent = `${visible.length} of ${jobs.length} downloads shown. ${active} active.`;
  }
  visible.forEach((job, index) => jobList.append(createJobCard(job, index === 0)));
  if (focusedAction?.jobId && focusedAction.action) {
    const replacement = Array.from(jobList.querySelectorAll<HTMLButtonElement>("button[data-job-id][data-action]")).find(
      (button) => button.dataset.jobId === focusedAction.jobId && button.dataset.action === focusedAction.action,
    );
    replacement?.focus({ preventScroll: true });
  }
}

function createJobCard(job: JobSnapshot, primary: boolean): HTMLElement {
  const article = document.createElement("article");
  article.className = "card job";
  article.dataset.state = job.state;
  article.dataset.jobId = job.jobId;

  const header = document.createElement("header");
  header.className = "job-header";
  const titleWrap = document.createElement("div");
  const source = document.createElement("p");
  source.className = "job-source";
  source.textContent = job.source;
  const heading = document.createElement("h3");
  heading.textContent = filename(job.destination) || "Download";
  titleWrap.append(heading, source);
  const status = document.createElement("output");
  status.className = "status";
  status.dataset.state = job.state;
  status.textContent = stateLabel(job.state);
  if (primary) status.id = "job-status";
  status.setAttribute("aria-live", "polite");
  header.append(titleWrap, status);
  article.append(header);

  const destination = document.createElement("p");
  destination.className = "destination";
  destination.textContent = job.destination ?? "Destination unavailable";
  article.append(destination);

  if (job.state === "scheduled" && job.notBeforeMs) {
    const schedule = document.createElement("p");
    schedule.className = "schedule-note";
    schedule.textContent = `Scheduled for ${new Date(job.notBeforeMs).toLocaleString()}`;
    article.append(schedule);
  }

  const progress = document.createElement("progress");
  progress.setAttribute("aria-label", `Progress for ${heading.textContent}`);
  if (primary) progress.id = "job-progress";
  if (job.state === "completed") {
    progress.max = Math.max(job.bytesReceived, 1);
    progress.value = job.bytesReceived;
  }
  article.append(progress);

  const bytes = document.createElement("output");
  bytes.className = "bytes";
  bytes.textContent = `${formatBytes(job.bytesReceived)} received`;
  bytes.setAttribute("aria-live", "polite");
  if (primary) bytes.id = "job-bytes";
  article.append(bytes);

  if (job.error) {
    const error = document.createElement("p");
    error.className = "error job-error";
    error.textContent = friendlyError(job);
    error.setAttribute("role", "alert");
    if (primary) error.id = "job-error";
    article.append(error);
  }

  if (job.observedSha256) {
    const completion = document.createElement("div");
    completion.className = "completion";
    const label = document.createElement("p");
    label.className = "digest-label";
    label.textContent = "Observed SHA-256";
    const digest = document.createElement("code");
    digest.textContent = job.observedSha256;
    if (primary) digest.id = "observed-hash";
    const note = document.createElement("p");
    note.className = "fine-print";
    note.textContent = "Observed locally; compare with a trusted publisher hash for authenticity.";
    completion.append(label, digest, note);
    article.append(completion);
  }

  const actions = document.createElement("div");
  actions.className = "job-actions";
  for (const action of actionsFor(job)) actions.append(actionButton(job, action));
  article.append(actions);
  return article;
}

function actionsFor(job: JobSnapshot): Array<{ action: string; label: string; danger?: boolean }> {
  const actions: Array<{ action: string; label: string; danger?: boolean }> = [];
  if (job.state === "scheduled") actions.push({ action: "start-now", label: "Start now" });
  if (["scheduled", "queued", "running", "cancelling"].includes(job.state)) {
    actions.push({ action: "cancel", label: job.state === "cancelling" ? "Cancelling…" : "Cancel", danger: true });
  }
  if (job.state === "failed" || job.state === "cancelled" || job.state === "needs_source") {
    if (job.action === "choose_new_path") actions.push({ action: "choose-new-path", label: "Choose new path" });
    else if (job.action === "edit_link") actions.push({ action: "edit-link", label: "Edit link" });
    else actions.push({ action: "retry", label: "Retry" });
  }
  if (job.state === "completed") actions.push({ action: "copy-path", label: "Copy path" });
  if (["completed", "cancelled", "failed"].includes(job.state)) actions.push({ action: "remove", label: "Remove", danger: false });
  return actions;
}

function actionButton(job: JobSnapshot, spec: { action: string; label: string; danger?: boolean }): HTMLButtonElement {
  const button = document.createElement("button");
  button.type = "button";
  button.className = spec.danger ? "secondary danger" : "secondary";
  button.dataset.action = spec.action;
  button.dataset.jobId = job.jobId;
  button.textContent = spec.label;
  if (job.state === "cancelling" && spec.action === "cancel") button.disabled = true;
  return button;
}

function beginEdit(job: JobSnapshot): void {
  editingJobId = job.jobId;
  urlInput.value = "";
  destinationInput.value = job.destination ?? "";
  scheduleInput.value = "";
  startButton.textContent = "Save and retry";
  formError.textContent = "Paste a refreshed address. Private query values are never restored from history.";
  formError.hidden = false;
  urlInput.focus();
  form.scrollIntoView({ behavior: "smooth", block: "start" });
}

function clearComposer(): void {
  editingJobId = null;
  form.reset();
  clearError(formError);
  renderPreview();
  startButton.textContent = "Add to queue";
}

function setComposerAvailability(available: boolean, label?: string): void {
  for (const control of form.querySelectorAll<HTMLInputElement | HTMLTextAreaElement | HTMLButtonElement>("input, textarea, button")) {
    control.disabled = !available;
  }
  if (label) startButton.textContent = label;
  else renderPreview();
}

function parseUrls(): string[] {
  return urlInput.value
    .split(/\r?\n/)
    .map((value) => value.trim())
    .filter(Boolean);
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
  if (filter === "scheduled") return job.state === "scheduled" || job.state === "queued";
  if (filter === "completed") return job.state === "completed";
  return job.state === "failed" || job.state === "needs_source";
}

function matchesSearch(job: JobSnapshot, query: string): boolean {
  if (!query) return true;
  return [job.source, job.destination, filename(job.destination), job.error]
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
    cancelling: "Cancelling",
    completed: "Complete",
    cancelled: "Cancelled",
    failed: "Needs attention",
    needs_source: "Link needed",
  }[state];
}

function friendlyError(job: JobSnapshot): string {
  if (job.action === "choose_new_path") return "A file already exists there. Choose a different destination to continue.";
  if (job.action === "edit_link") return "This link needs attention. Paste a refreshed address to continue safely.";
  return job.error ?? "The download stopped. Retry when the source is available.";
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

function suggestedFilename(value: string): string {
  try {
    const segments = new URL(value).pathname.split("/").filter(Boolean);
    const segment = segments[segments.length - 1];
    return sanitizeFilename(segment ? decodeURIComponent(segment) : "download.bin");
  } catch {
    return "download.bin";
  }
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
  element.textContent = error instanceof Error ? error.message : typeof error === "string" ? error : "Something went wrong. Please try again.";
  element.hidden = false;
}

function clearError(element: HTMLElement): void {
  element.textContent = "";
  element.hidden = true;
}

function required<T extends HTMLElement>(id: string): T {
  const element = document.getElementById(id);
  if (!element) throw new Error(`Missing required element: ${id}`);
  return element as T;
}

renderPreview();
void refreshQueue();
refreshTimer = window.setInterval(() => void refreshQueue(), 350);
window.addEventListener("beforeunload", () => window.clearInterval(refreshTimer));
