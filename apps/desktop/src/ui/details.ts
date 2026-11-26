import type { EngineApi } from "../engine/api";
import type { JobDetails, JobSnapshot, SegmentView } from "../engine/types";
import { type QueueCapabilities, actionButton, actionsFor } from "./actions";
import { renderSpeedChart } from "./chart";
import { glyph, required, setStatus } from "./dom";
import {
  type SpeedSample,
  computeSpeedStats,
  fileKindMeta,
  filename,
  formatBytes,
  formatDurationLong,
  friendlyError,
} from "./format";

export interface DetailsContext {
  engine: EngineApi;
  capabilities: QueueCapabilities;
  openDialog(dialog: HTMLDialogElement, focus: HTMLElement): void;
  restoreFocus(fallback: HTMLElement, dialog?: HTMLDialogElement): void;
  /** The row's Details button, which gets focus back when the dialog closes. */
  rowDetailsButton(jobId: string): HTMLElement | null;
  /** Where focus goes when the row is gone. */
  fallbackFocus: HTMLElement;
  announce(message: string): void;
  onAction(job: JobSnapshot, action: string, trigger?: HTMLButtonElement): Promise<void>;
}

export interface DetailsView {
  isOpen(): boolean;
  close(): void;
  /** Samples the engine-measured rate of every running download. Call on each queue refresh. */
  recordSpeeds(snapshot: JobSnapshot[]): void;
  open(jobId: string, opener: HTMLElement | null): Promise<void>;
  /** Redraws an open dialog. Returns false when the download is gone, having closed the window. */
  refresh(): Promise<boolean>;
}

/** The details dialog: figures, speed graph, file map and facts. Mount its markup first. */
export function createDetailsView(ctx: DetailsContext): DetailsView {
  const { engine } = ctx;
  const detailsDialog = required<HTMLDialogElement>("details-dialog");
  const detailsKindIcon = required<HTMLElement>("details-kind-icon");
  const detailsTitle = required<HTMLElement>("details-title");
  const detailsSource = required<HTMLElement>("details-source");
  const detailsStatus = required<HTMLOutputElement>("details-status");
  const detailsFigures = required<HTMLElement>("details-figures");
  const detailsSpeedStats = required<HTMLElement>("details-speed-stats");
  const detailsGraph = required<SVGSVGElement>("details-graph");
  const detailsGraphLabel = required<SVGTitleElement>("details-graph-label");
  const detailsGraphNote = required<HTMLElement>("details-graph-note");
  const detailsMap = required<SVGSVGElement>("details-map");
  const detailsMapLabel = required<SVGTitleElement>("details-map-label");
  const detailsSegments = required<HTMLOListElement>("details-segments");
  const detailsSegmentsNote = required<HTMLElement>("details-segments-note");
  const detailsInfo = required<HTMLElement>("details-info");
  const detailsActions = required<HTMLDivElement>("details-actions");
  const detailsClose = required<HTMLButtonElement>("details-close");
  const detailsCloseX = required<HTMLButtonElement>("details-close-x");

  /** Maximum samples kept per download (frozen once a download stops so history stays visible). */
  const MAX_SAMPLES = 180;

  /**
   * Speed samples per download, taken from the engine-measured rate on every
   * queue refresh. Samples remain frozen after completion/pause so inspecting
   * a finished download still displays its transfer speed history.
   */
  const speedHistory = new Map<string, SpeedSample[]>();
  let detailsJobId: string | null = null;
  let currentJob: JobSnapshot | null = null;
  let renderedActionsSignature = "";

  /* Details window ------------------------------------------------------------
     Everything here is drawn from engine-reported values: the smoothed rate the
     queue already shows, bytes written, and the ranges in flight. Nothing is
     interpolated between samples, and a stopped download shows no live speed. */

  function recordSpeeds(snapshot: JobSnapshot[]): void {
    const now = Date.now();
    const present = new Set<string>();
    for (const job of snapshot) {
      present.add(job.jobId);
      if (job.state !== "running") continue;
      const samples = speedHistory.get(job.jobId) ?? [];
      samples.push({ at: now, bytesPerSecond: job.bytesPerSecond ?? 0 });
      if (samples.length > MAX_SAMPLES) samples.shift();
      speedHistory.set(job.jobId, samples);
    }
    for (const jobId of speedHistory.keys()) if (!present.has(jobId)) speedHistory.delete(jobId);
  }

  async function openDetails(jobId: string, opener: HTMLElement | null): Promise<void> {
    detailsJobId = jobId;
    renderedActionsSignature = "";
    if (!(await refreshDetails())) return;
    // A click on the card leaves focus on the body; return it to the row instead.
    if (opener === null) ctx.rowDetailsButton(jobId)?.focus({ preventScroll: true });
    // Focus the top-right close button (not the footer button) so the dialog
    // always opens at the very top showing the title, KPIs, and speed history.
    ctx.openDialog(detailsDialog, detailsCloseX);
  }

  detailsClose.addEventListener("click", () => detailsDialog.close());
  detailsCloseX.addEventListener("click", () => detailsDialog.close());
  detailsDialog.addEventListener("close", () => {
    const jobId = detailsJobId;
    detailsJobId = null;
    currentJob = null;
    renderedActionsSignature = "";
    const button = jobId
      ? ctx.rowDetailsButton(jobId)
      : null;
    ctx.restoreFocus(button ?? ctx.fallbackFocus, detailsDialog);
  });

  detailsActions.addEventListener("click", async (event) => {
    const button = (event.target as HTMLElement).closest<HTMLButtonElement>("button[data-action][data-job-id]");
    if (!button || !currentJob) return;
    const action = button.dataset.action!;
    // Actions that open another dialog or delete the item close Details first so
    // modals do not stack unnecessarily.
    if (["edit-link", "edit-checksum", "configure-media"].includes(action)) {
      const targetJob = currentJob;
      detailsDialog.close();
      await ctx.onAction(targetJob, action, button);
      return;
    }
    await ctx.onAction(currentJob, action, button);
    if (detailsDialog.open) await refreshDetails();
  });

  /** Returns false when the download is gone, having closed the window. */
  async function refreshDetails(): Promise<boolean> {
    if (!detailsJobId) return false;
    let details: JobDetails;
    try {
      details = await engine.downloadDetails(detailsJobId);
    } catch {
      if (detailsDialog.open) detailsDialog.close();
      return false;
    }
    renderDetails(details);
    return true;
  }

  function renderDetailsActions(job: JobSnapshot): void {
    const specs = actionsFor(job, ctx.capabilities).filter((spec) => spec.action !== "details");
    const signature = JSON.stringify([job.jobId, job.state, job.action ?? "", specs.map((s) => `${s.action}:${s.label}`)]);
    if (signature === renderedActionsSignature) return;
    renderedActionsSignature = signature;
    detailsActions.replaceChildren(...specs.map((spec) => actionButton(job, spec)));
  }

  function renderDetails({ job, segments }: JobDetails): void {
    currentJob = job;
    const name = filename(job.destination) || "Download";
    const meta = fileKindMeta(job);
    detailsKindIcon.dataset.kind = meta.category;
    detailsKindIcon.replaceChildren(glyph(meta.icon));
    detailsTitle.textContent = name;
    detailsSource.textContent = job.source;
    detailsSource.title = job.source;
    setStatus(detailsStatus, job);
    renderDetailsActions(job);

    const running = job.state === "running";
    const completed = job.state === "completed";
    const known = job.totalBytes && job.totalBytes > 0 ? job.totalBytes : null;
    const inFlight = segments.reduce((sum, segment) => sum + segment.received, 0);
    const samples = speedHistory.get(job.jobId) ?? [];
    const stats = computeSpeedStats(samples);
    const elapsedSec =
      completed && job.finishedAtMs && job.finishedAtMs > job.createdAtMs
        ? Math.max(1, Math.round((job.finishedAtMs - job.createdAtMs) / 1000))
        : null;

    let figures: Array<[string, string]>;
    if (completed) {
      const avgLabel = stats.mean > 0 ? `${formatBytes(stats.mean)}/s` : "—";
      const medLabel = stats.median > 0 ? `${formatBytes(stats.median)}/s` : "—";
      const peakLabel = stats.peak > 0 ? `${formatBytes(stats.peak)}/s` : "—";
      figures = [
        ["File size", formatBytes(known ?? job.bytesReceived)],
        ["Average speed", avgLabel],
        ["Median speed", medLabel],
        ["Peak speed", peakLabel],
        ["Duration", elapsedSec ? formatDurationLong(elapsedSec) : "Under 1 sec"],
      ];
    } else if (running) {
      figures = [
        ["Current speed", job.bytesPerSecond ? `${formatBytes(job.bytesPerSecond)}/s` : "Measuring…"],
        ["Median speed", stats.median > 0 ? `${formatBytes(stats.median)}/s` : "—"],
        [
          "Received",
          known
            ? `${formatBytes(job.bytesReceived)} / ${formatBytes(known)}`
            : formatBytes(job.bytesReceived),
        ],
        ["Progress", known ? `${Math.floor(Math.min(job.bytesReceived / known, 1) * 100)}%` : "Unknown"],
        ["Time left", job.etaSeconds != null ? formatDurationLong(job.etaSeconds) : "—"],
        ["Connections", String(Math.max(segments.length, 1))],
      ];
    } else {
      figures = [
        [
          "Received",
          known
            ? `${formatBytes(job.bytesReceived)} / ${formatBytes(known)}`
            : formatBytes(job.bytesReceived),
        ],
        ["Progress", known ? `${Math.floor(Math.min(job.bytesReceived / known, 1) * 100)}%` : "—"],
        ["Average speed", stats.mean > 0 ? `${formatBytes(stats.mean)}/s` : "—"],
        ["Peak speed", stats.peak > 0 ? `${formatBytes(stats.peak)}/s` : "—"],
        ["Retries", String(job.attempt)],
      ];
    }
    detailsFigures.replaceChildren(...figures.map(([term, value]) => figureGroup(term, value)));

    renderSpeedGraph(job);
    renderFileMap(job, segments, known, inFlight);
    renderDetailsInfo(job);
  }

  function figureGroup(term: string, value: string): HTMLElement {
    const group = document.createElement("div");
    group.className = "kpi-tile";
    const dt = document.createElement("dt");
    dt.textContent = term;
    const dd = document.createElement("dd");
    dd.textContent = value;
    dd.title = value;
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
    const samples = speedHistory.get(job.jobId) ?? [];
    const live = job.state === "running";
    const emptyMessage = live
      ? "Measuring transfer speed…"
      : job.reusedFromCache
        ? "Reused directly from this computer's verified cache (no network transfer)."
        : "No transfer speed samples were recorded during this window's session.";
    renderSpeedChart({
      svg: detailsGraph,
      title: detailsGraphLabel,
      statsEl: detailsSpeedStats,
      noteEl: detailsGraphNote,
      samples,
      live,
      liveWindowMs: 90_000,
      emptyMessage,
    });
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
    const rows: Array<{ term: string; value: string; mono?: boolean; copyValue?: string; copyLabel?: string }> = [
      {
        term: "Saved to",
        value: job.destination ?? "Not chosen yet",
        mono: Boolean(job.destination),
        copyValue: job.destination ?? undefined,
        copyLabel: "Copy destination path",
      },
      {
        term: "Total size",
        value: job.totalBytes === null ? "Not stated by the source" : `${formatBytes(job.totalBytes)} (${job.totalBytes.toLocaleString()} bytes)`,
      },
      {
        term: "Kind",
        value: job.kind === "torrent" ? "Torrent" : job.kind === "media" ? `Media${job.qualityLabel ? ` · ${job.qualityLabel}` : ""}` : "File",
      },
      { term: "Added", value: new Date(job.createdAtMs).toLocaleString() },
    ];
    if (job.finishedAtMs) {
      rows.push({ term: "Finished", value: new Date(job.finishedAtMs).toLocaleString() });
      // Includes any time spent waiting in the queue, so no average speed is
      // derived from it.
      const elapsed = Math.round((job.finishedAtMs - job.createdAtMs) / 1000);
      if (elapsed > 0) rows.push({ term: "Added to finished", value: formatDurationLong(elapsed) });
    }
    if (job.attempt > 0) rows.push({ term: "Automatic retries", value: String(job.attempt) });
    if (job.observedSha256) {
      const matched = job.state === "completed" && !!job.expectedSha256;
      rows.push({
        term: matched ? "SHA-256 (matched)" : "Observed SHA-256",
        value: job.observedSha256,
        mono: true,
        copyValue: job.observedSha256,
        copyLabel: "Copy SHA-256",
      });
    }
    if (job.error) rows.push({ term: "Problem", value: friendlyError(job) });
    detailsInfo.replaceChildren(
      ...rows.flatMap(({ term, value, mono, copyValue, copyLabel }) => {
        const dt = document.createElement("dt");
        dt.textContent = term;
        const dd = document.createElement("dd");
        const content = mono ? document.createElement("code") : document.createElement("span");
        content.textContent = value;
        if (copyValue && navigator.clipboard) {
          dd.className = "details-copy-row";
          const copy = document.createElement("button");
          copy.type = "button";
          copy.className = "quiet copy-inline";
          copy.textContent = "Copy";
          copy.setAttribute("aria-label", copyLabel ?? `Copy ${term}`);
          copy.addEventListener("click", async () => {
            try {
              await navigator.clipboard.writeText(copyValue);
              copy.textContent = "Copied";
              ctx.announce(`${term} copied.`);
              window.setTimeout(() => {
                if (copy.isConnected) copy.textContent = "Copy";
              }, 1500);
            } catch {
              ctx.announce(`Could not copy ${term.toLowerCase()}.`);
            }
          });
          dd.append(content, copy);
        } else {
          dd.append(content);
        }
        return [dt, dd];
      }),
    );
  }

  return {
    isOpen: () => detailsDialog.open,
    close() {
      if (detailsDialog.open) detailsDialog.close();
    },
    recordSpeeds,
    open: openDetails,
    refresh: refreshDetails,
  };
}
