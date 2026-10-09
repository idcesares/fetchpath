import type { EngineApi } from "../engine/api";
import type { JobDetails, JobSnapshot, SegmentView } from "../engine/types";
import { required, setStatus } from "./dom";
import { filename, formatBytes, formatDurationLong, friendlyError } from "./format";

export interface DetailsContext {
  engine: EngineApi;
  openDialog(dialog: HTMLDialogElement, focus: HTMLElement): void;
  restoreFocus(fallback: HTMLElement): void;
  /** The row's Details button, which gets focus back when the dialog closes. */
  rowDetailsButton(jobId: string): HTMLElement | null;
  /** Where focus goes when the row is gone. */
  fallbackFocus: HTMLElement;
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

  /** How far back the speed graph looks. */
  const SPEED_WINDOW_MS = 60_000;

  /**
   * Speed samples per running download, taken from the engine-measured rate on
   * every queue refresh, so the graph already has history when it is opened.
   * Live only; a restart starts a new history.
   */
  const speedHistory = new Map<string, Array<{ at: number; bytesPerSecond: number }>>();
  let detailsJobId: string | null = null;

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
    if (opener === null) ctx.rowDetailsButton(jobId)?.focus();
    ctx.openDialog(detailsDialog, detailsClose);
  }

  detailsClose.addEventListener("click", () => detailsDialog.close());
  detailsDialog.addEventListener("close", () => {
    const jobId = detailsJobId;
    detailsJobId = null;
    const button = jobId
      ? ctx.rowDetailsButton(jobId)
      : null;
    ctx.restoreFocus(button ?? ctx.fallbackFocus);
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

  function renderDetails({ job, segments }: JobDetails): void {
    const name = filename(job.destination) || "Download";
    detailsTitle.textContent = name;
    detailsSource.textContent = job.source;
    detailsSource.title = job.source;
    setStatus(detailsStatus, job);

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
