import { type SpeedSample, type SpeedStats, computeSpeedStats, formatBytes, formatDurationLong } from "./format";

const SVG_NS = "http://www.w3.org/2000/svg";

function svgEl<K extends keyof SVGElementTagNameMap>(
  tag: K,
  attributes: Record<string, string | number>,
  text?: string,
): SVGElementTagNameMap[K] {
  const element = document.createElementNS(SVG_NS, tag);
  for (const [key, value] of Object.entries(attributes)) element.setAttribute(key, String(value));
  if (text !== undefined) element.textContent = text;
  return element;
}

export interface SpeedChartOptions {
  svg: SVGSVGElement;
  title: SVGTitleElement;
  statsEl: HTMLElement;
  noteEl: HTMLElement;
  samples: readonly SpeedSample[];
  /** True while the download (or session) is actively transferring. */
  live: boolean;
  /** Fixed sliding window for live charts; finished charts fit their exact recorded span. */
  liveWindowMs?: number;
  /** Empty-state explanation when no transfer samples exist yet. */
  emptyMessage: string;
  /** Optional fallback observed speed (e.g. fast transfer that completed between 1s polls). */
  fallbackSpeed?: number | null;
  fallbackNote?: string;
}

/**
 * Renders a speed history SVG chart with area fill, horizontal mean/average
 * reference line, peak/median/average badges, and scale markers.
 */
export function renderSpeedChart(opts: SpeedChartOptions): SpeedStats {
  const { svg, title, statsEl, noteEl, samples, live, liveWindowMs = 60_000 } = opts;
  const viewBox = svg.getAttribute("viewBox")?.split(/\s+/).map(Number) ?? [0, 0, 640, 165];
  const width = viewBox[2] || 640;
  const height = viewBox[3] || 165;
  const padTop = 12;
  const padBottom = 18;
  const plotHeight = height - padTop - padBottom;

  const gridLines = [0, 0.5, 1].map((fraction) =>
    svgEl("line", {
      x1: 0,
      x2: width,
      y1: (padTop + plotHeight * fraction).toFixed(1),
      y2: (padTop + plotHeight * fraction).toFixed(1),
      class: "graph-grid",
    }),
  );

  const stats = computeSpeedStats(samples);
  const hasRealCurve = samples.length >= 2 && stats.peak > 0;
  const singleSpeed = !hasRealCurve ? (stats.peak > 0 ? stats.peak : opts.fallbackSpeed ?? 0) : 0;

  if (!hasRealCurve && singleSpeed <= 0) {
    svg.replaceChildren(title, ...gridLines);
    statsEl.replaceChildren();
    title.textContent = opts.emptyMessage;
    noteEl.textContent = opts.emptyMessage;
    return stats;
  }

  // Headroom above peak so the curve never clips against the top edge.
  const peakRef = hasRealCurve ? stats.peak : singleSpeed;
  const scale = peakRef * 1.15;
  const y = (rate: number) => padTop + plotHeight - (Math.min(rate, scale) / scale) * plotHeight;

  const elements: SVGElement[] = [...gridLines];
  const baselineY = padTop + plotHeight;

  if (hasRealCurve) {
    const firstAt = samples[0].at;
    const lastAt = samples[samples.length - 1].at;
    const windowEnd = live ? Math.max(Date.now(), lastAt) : lastAt;
    const windowStart = live
      ? Math.min(firstAt, windowEnd - liveWindowMs)
      : firstAt;
    const span = Math.max(windowEnd - windowStart, 1000);
    const x = (at: number) => Math.max(0, Math.min(width, ((at - windowStart) / span) * width));

    const pts = samples.map((s) => `${x(s.at).toFixed(1)},${y(s.bytesPerSecond).toFixed(1)}`);
    const firstX = x(samples[0].at).toFixed(1);
    const lastX = x(samples[samples.length - 1].at).toFixed(1);
    const lastY = y(samples[samples.length - 1].bytesPerSecond).toFixed(1);

    elements.push(
      svgEl("polygon", {
        points: `${firstX},${baselineY} ${pts.join(" ")} ${lastX},${baselineY}`,
        class: "graph-area",
      }),
    );

    // Dashed horizontal average (mean) line for immediate comparison against spikes.
    if (stats.mean > 0) {
      const meanY = y(stats.mean).toFixed(1);
      elements.push(
        svgEl("line", {
          x1: 0,
          x2: width,
          y1: meanY,
          y2: meanY,
          class: "graph-mean",
        }),
      );
    }

    elements.push(svgEl("polyline", { points: pts.join(" "), class: "graph-line" }));
    // Waypoint head dot on the latest recorded sample.
    elements.push(
      svgEl("circle", {
        cx: lastX,
        cy: lastY,
        r: 4,
        class: "graph-head",
      }),
    );

    const spanSec = Math.max(1, Math.round((lastAt - firstAt) / 1000));
    const statItems: Array<[string, string, string?]> = [
      ["Average", `${formatBytes(stats.mean)}/s`, "mean"],
      ["Median", `${formatBytes(stats.median)}/s`],
      ["Peak", `${formatBytes(stats.peak)}/s`],
    ];
    if (live && stats.latest > 0) statItems.unshift(["Now", `${formatBytes(stats.latest)}/s`, "now"]);
    renderStatPills(statsEl, statItems);

    const summary = live
      ? `Observed over ${formatDurationLong(spanSec)} · dashed line marks average (${formatBytes(stats.mean)}/s) · top scale ${formatBytes(Math.round(scale))}/s`
      : `Recorded over ${formatDurationLong(spanSec)} · dashed line marks average (${formatBytes(stats.mean)}/s) · top scale ${formatBytes(Math.round(scale))}/s`;
    title.textContent = `Speed history. Average ${formatBytes(stats.mean)}/s, median ${formatBytes(stats.median)}/s, peak ${formatBytes(stats.peak)}/s.`;
    noteEl.textContent = summary;
  } else {
    // Fast transfer with a single observed rate: draw a crisp horizontal average bar.
    const barY = y(singleSpeed).toFixed(1);
    elements.push(
      svgEl("rect", {
        x: 0,
        y: barY,
        width,
        height: Math.max(1, baselineY - Number(barY)),
        class: "graph-area",
      }),
      svgEl("line", {
        x1: 0,
        x2: width,
        y1: barY,
        y2: barY,
        class: "graph-mean",
      }),
    );
    renderStatPills(statsEl, [["Average", `${formatBytes(singleSpeed)}/s`, "mean"]]);
    const note = opts.fallbackNote ?? `Completed quickly at about ${formatBytes(singleSpeed)}/s.`;
    title.textContent = note;
    noteEl.textContent = note;
  }

  svg.replaceChildren(title, ...elements);
  return stats;
}

function renderStatPills(container: HTMLElement, items: Array<[string, string, string?]>): void {
  container.replaceChildren(
    ...items.map(([label, value, kind]) => {
      const wrap = document.createElement("div");
      if (kind) wrap.dataset.kind = kind;
      const dt = document.createElement("dt");
      dt.textContent = label;
      const dd = document.createElement("dd");
      dd.textContent = value;
      wrap.append(dt, dd);
      return wrap;
    }),
  );
}
