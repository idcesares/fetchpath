import type { JobSnapshot, JobState } from "../engine/types";
import { jobLabel } from "./format";

export function required<T extends Element>(id: string): T {
  const element = document.getElementById(id);
  if (!element) throw new Error(`Missing required element: ${id}`);
  return element as Element as T;
}

/** The glyph that accompanies each state, so a chip is never colour alone. */
const STATE_GLYPH: Record<JobState, string> = {
  scheduled: "i-wait",
  queued: "i-wait",
  running: "i-run",
  paused: "i-pause",
  cancelling: "i-pause",
  completed: "i-ok",
  cancelled: "i-fail",
  failed: "i-fail",
  needs_source: "i-fail",
  awaiting_approval: "i-appr",
};

/** A decorative glyph, then the word. The glyph is hidden from assistive technology. */
function glyph(id: string): SVGSVGElement {
  const svg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
  svg.setAttribute("class", "i");
  svg.setAttribute("aria-hidden", "true");
  const use = document.createElementNS("http://www.w3.org/2000/svg", "use");
  use.setAttribute("href", "#" + id);
  svg.append(use);
  return svg;
}

export function setStatus(element: HTMLElement, job: JobSnapshot): void {
  const label = jobLabel(job);
  if (element.dataset.state === job.state && element.dataset.label === label && element.childNodes.length > 0) return;
  element.dataset.state = job.state;
  element.dataset.label = label;
  element.replaceChildren(glyph(STATE_GLYPH[job.state]), document.createTextNode(label));
}
