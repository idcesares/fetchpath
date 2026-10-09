/*
 * Markup shared by every entry that shows the queue and a download's details.
 * Static strings only: no job data is ever interpolated here, and there is no
 * inline script, style or event handler, so a page under `default-src 'self'`
 * can load it unchanged. Each entry's HTML holds a
 * `<template data-mount="...">` where a piece belongs, and `mountSharedMarkup`
 * swaps it for the real nodes in place, so the page ends up as it was written
 * out by hand.
 */

/** The "Downloads" heading with the search box, and the filter chips. */
export const queueTopMarkup = `
  <section class="queue-heading" aria-labelledby="queue-title">
    <h1 id="queue-title" tabindex="-1">Downloads</h1>
    <label class="search" for="queue-search">
      <span class="sr-only">Search downloads</span>
      <input id="queue-search" type="search" autocomplete="off" placeholder="Search files or sources" />
    </label>
  </section>

  <div id="queue-filters" class="filters" role="group" aria-label="Filter downloads">
    <button class="filter is-selected" type="button" data-filter="all" aria-pressed="true">All</button>
    <button class="filter" type="button" data-filter="active" aria-pressed="false">Active</button>
    <button class="filter" type="button" data-filter="paused" aria-pressed="false">Paused</button>
    <button class="filter" type="button" data-filter="scheduled" aria-pressed="false">Scheduled</button>
    <button class="filter" type="button" data-filter="completed" aria-pressed="false">Completed</button>
    <button class="filter" type="button" data-filter="failed" aria-pressed="false">Needs attention</button>
  </div>
`;

/** The summary and bulk actions, the empty state and the list of downloads. */
export const queueBodyMarkup = `
  <div class="queue-status-row">
    <p id="queue-summary" class="queue-summary">No downloads yet.</p>
    <div class="queue-bulk-actions">
      <button id="pause-all" class="secondary" type="button" hidden>Pause all</button>
      <button id="resume-all" class="secondary" type="button" hidden>Resume all</button>
      <button id="cancel-download" class="secondary danger" type="button" hidden>Cancel current download</button>
    </div>
  </div>

  <section id="queue-empty" class="empty-state" aria-labelledby="empty-title" hidden>
    <span class="mark large" aria-hidden="true"><svg class="i" width="28" height="28"><use href="#i-logo" /></svg></span>
    <h2 id="empty-title">Nothing downloading yet</h2>
    <p id="empty-hint">Copy a link and paste it here with <kbd>Ctrl</kbd> <kbd>V</kbd>, or add one yourself.</p>
    <button id="empty-add" type="button" aria-haspopup="dialog"><svg class="i" aria-hidden="true"><use href="#i-plus" /></svg>Add a download</button>
    <p id="empty-reassure" class="reassure">Large files resume if you close the app.</p>
  </section>

  <section id="job-card" class="queue" aria-label="Download queue" hidden>
    <div id="job-list" role="list"></div>
  </section>
`;

/** The details dialog. */
export const detailsDialogMarkup = `
  <dialog id="details-dialog" class="card dialog details-dialog" aria-labelledby="details-title">
    <header class="details-header">
      <div>
        <h2 id="details-title">Download details</h2>
        <p id="details-source" class="job-source"></p>
      </div>
      <output id="details-status" class="status"></output>
    </header>

    <dl id="details-figures" class="stats-grid details-figures" aria-live="off"></dl>

    <section class="details-section" aria-labelledby="details-speed-title">
      <h3 id="details-speed-title">Speed</h3>
      <svg id="details-graph" class="speed-graph" role="img" viewBox="0 0 600 140" preserveAspectRatio="none"
           aria-labelledby="details-graph-label">
        <title id="details-graph-label">Speed over the last minute</title>
      </svg>
      <p id="details-graph-note" class="hint"></p>
    </section>

    <section class="details-section" aria-labelledby="details-map-title">
      <h3 id="details-map-title">File</h3>
      <svg id="details-map" class="file-map" role="img" viewBox="0 0 600 18" preserveAspectRatio="none"
           aria-labelledby="details-map-label">
        <title id="details-map-label">Which parts of the file have arrived</title>
      </svg>
      <p class="hint legend">
        <span class="swatch written" aria-hidden="true"></span> Written to disk
        <span class="swatch inflight" aria-hidden="true"></span> Arriving now
        <span class="swatch pending" aria-hidden="true"></span> Not yet requested
      </p>
      <ol id="details-segments" class="segments" aria-label="Connections"></ol>
      <p id="details-segments-note" class="hint"></p>
    </section>

    <dl id="details-info" class="details-info"></dl>

    <div class="dialog-actions">
      <button id="details-close" type="button">Close</button>
    </div>
  </dialog>
`;

const PIECES: Record<string, string> = {
  "queue-top": queueTopMarkup,
  "queue-body": queueBodyMarkup,
  "details-dialog": detailsDialogMarkup,
};

/**
 * Replaces every `<template data-mount>` under `root` with its markup. Must run
 * before any code looks the elements up by id.
 */
export function mountSharedMarkup(root: ParentNode = document): void {
  for (const placeholder of root.querySelectorAll<HTMLTemplateElement>("template[data-mount]")) {
    const markup = PIECES[placeholder.dataset.mount ?? ""];
    if (markup === undefined) continue;
    const holder = document.createElement("template");
    holder.innerHTML = markup.trim();
    placeholder.replaceWith(holder.content);
  }
}
