/*
 * Markup shared by every entry that shows the queue and a download's details.
 * Static strings only: no job data is ever interpolated here, and there is no
 * inline script, style or event handler, so a page under `default-src 'self'`
 * can load it unchanged. Each entry's HTML holds a
 * `<template data-mount="...">` where a piece belongs, and `mountSharedMarkup`
 * swaps it for the real nodes in place, so the page ends up as it was written
 * out by hand.
 */

/** The "Downloads" heading with the search box, filters, sort and view controls. */
export const queueTopMarkup = `
  <section class="queue-heading" aria-labelledby="queue-title">
    <h1 id="queue-title" tabindex="-1">Downloads</h1>
    <label class="search" for="queue-search">
      <svg class="i search-icon" aria-hidden="true"><use href="#i-search" /></svg>
      <span class="sr-only">Search downloads</span>
      <input id="queue-search" type="search" autocomplete="off" placeholder="Search files or sources" />
    </label>
  </section>

  <div id="queue-toolbar" class="queue-toolbar" hidden>
    <div id="queue-filters" class="filters" role="group" aria-label="Filter downloads">
      <button class="filter" type="button" data-filter="all" aria-pressed="true">All <span class="filter-count" aria-hidden="true"></span></button>
      <button class="filter" type="button" data-filter="active" aria-pressed="false">Active <span class="filter-count" aria-hidden="true"></span></button>
      <button class="filter" type="button" data-filter="paused" aria-pressed="false">Paused <span class="filter-count" aria-hidden="true"></span></button>
      <button class="filter" type="button" data-filter="scheduled" aria-pressed="false">Scheduled <span class="filter-count" aria-hidden="true"></span></button>
      <button class="filter" type="button" data-filter="completed" aria-pressed="false">Completed <span class="filter-count" aria-hidden="true"></span></button>
      <button class="filter" type="button" data-filter="failed" aria-pressed="false">Needs attention <span class="filter-count" aria-hidden="true"></span></button>
    </div>

    <div class="queue-controls">
      <label class="sort-wrap" for="queue-sort">
        <span class="sr-only">Sort downloads</span>
        <select id="queue-sort" class="sort-select" aria-label="Sort downloads">
          <option value="newest">Newest first</option>
          <option value="active">Active first</option>
          <option value="name">Name (A–Z)</option>
          <option value="size">Largest first</option>
          <option value="progress">Progress</option>
        </select>
      </label>

      <button id="toggle-speed-chart" class="quiet chart-toggle" type="button" aria-pressed="false" aria-controls="session-speed-panel" title="Toggle live speed history chart">
        <svg class="i" aria-hidden="true"><use href="#i-activity" /></svg><span>Speed</span>
      </button>

      <div id="view-modes" class="view-modes" role="group" aria-label="Display mode">
        <button class="view-mode" type="button" data-view="cards" aria-pressed="true" aria-label="Cards view" title="Cards view">
          <svg class="i" aria-hidden="true"><use href="#i-view-cards" /></svg>
        </button>
        <button class="view-mode" type="button" data-view="compact" aria-pressed="false" aria-label="Compact list view" title="Compact list">
          <svg class="i" aria-hidden="true"><use href="#i-view-compact" /></svg>
        </button>
        <button class="view-mode" type="button" data-view="grid" aria-pressed="false" aria-label="Grid view" title="Grid view">
          <svg class="i" aria-hidden="true"><use href="#i-view-grid" /></svg>
        </button>
        <button class="view-mode" type="button" data-view="table" aria-pressed="false" aria-label="Table view" title="Table view">
          <svg class="i" aria-hidden="true"><use href="#i-view-table" /></svg>
        </button>
      </div>
    </div>
  </div>
`;

/** The summary and bulk actions, live session chart, empty states and list of downloads. */
export const queueBodyMarkup = `
  <section id="session-speed-panel" class="card speed-panel" aria-labelledby="session-speed-title" hidden>
    <div class="chart-header">
      <div>
        <h2 id="session-speed-title">Live speed history</h2>
        <p id="session-speed-note" class="hint">Combined transfer speed observed across active downloads.</p>
      </div>
      <dl id="session-speed-stats" class="chart-stats"></dl>
    </div>
    <svg id="session-graph" class="speed-graph session-graph" role="img" viewBox="0 0 640 130" preserveAspectRatio="none"
         aria-labelledby="session-graph-label">
      <title id="session-graph-label">Combined download speed over time</title>
    </svg>
  </section>

  <div class="queue-status-row">
    <p id="queue-summary" class="queue-summary">No downloads yet.</p>
    <div class="queue-bulk-actions">
      <button id="pause-all" class="secondary" type="button" hidden><svg class="i" aria-hidden="true"><use href="#i-pause" /></svg>Pause all</button>
      <button id="resume-all" class="secondary" type="button" hidden><svg class="i" aria-hidden="true"><use href="#i-play" /></svg>Resume all</button>
      <button id="clear-completed" class="quiet" type="button" hidden><svg class="i" aria-hidden="true"><use href="#i-trash" /></svg>Clear completed</button>
    </div>
  </div>

  <section id="queue-empty" class="empty-state" aria-labelledby="empty-title" hidden>
    <span class="mark large" aria-hidden="true"><svg class="i" width="28" height="28"><use href="#i-logo" /></svg></span>
    <h2 id="empty-title">Nothing downloading yet</h2>
    <p id="empty-hint">Copy a link and paste it here with <kbd>Ctrl</kbd> <kbd>V</kbd>, drag a link or torrent here, or add one yourself.</p>
    <button id="empty-add" type="button" aria-haspopup="dialog"><svg class="i" aria-hidden="true"><use href="#i-plus" /></svg>Add a download</button>
    <p id="empty-reassure" class="reassure">Large files resume if you close the app.</p>
  </section>

  <section id="queue-no-match" class="empty-state empty-filter" aria-labelledby="no-match-title" hidden>
    <h2 id="no-match-title">No downloads match this filter</h2>
    <p>Try another filter or clear the search box to see the rest of your queue.</p>
    <button id="reset-filters" class="secondary" type="button">Show all downloads</button>
  </section>

  <section id="job-card" class="queue" data-view="cards" aria-label="Download queue" hidden>
    <div id="table-head" class="table-head" aria-hidden="true" hidden>
      <span>File</span>
      <span>Status</span>
      <span>Progress</span>
      <span>Size</span>
      <span>Speed / ETA</span>
      <span class="table-head-actions">Actions</span>
    </div>
    <div id="job-list" role="list"></div>
  </section>

  <dialog id="confirm-dialog" class="card dialog confirm-dialog" role="alertdialog" aria-labelledby="confirm-title" aria-describedby="confirm-detail">
    <div class="dialog-header">
      <h2 id="confirm-title">Confirm</h2>
      <button id="confirm-close" class="quiet dialog-close" type="button" aria-label="Close"><svg class="i" aria-hidden="true"><use href="#i-close" /></svg></button>
    </div>
    <div class="dialog-body">
      <p id="confirm-detail"></p>
      <p id="confirm-note" class="hint" hidden></p>
    </div>
    <div class="dialog-actions">
      <button id="confirm-cancel" class="quiet" type="button">Keep</button>
      <button id="confirm-accept" class="secondary danger" type="button">Remove</button>
    </div>
  </dialog>
`;

/** The details dialog. */
export const detailsDialogMarkup = `
  <dialog id="details-dialog" class="card dialog details-dialog" aria-labelledby="details-title">
    <header class="details-header">
      <div class="details-title-row">
        <span id="details-kind-icon" class="kind-badge" aria-hidden="true"><svg class="i"><use href="#i-file" /></svg></span>
        <div>
          <h2 id="details-title">Download details</h2>
          <p id="details-source" class="job-source"></p>
        </div>
      </div>
      <div class="details-header-end">
        <output id="details-status" class="status"></output>
        <button id="details-close-x" class="quiet dialog-close" type="button" aria-label="Close"><svg class="i" aria-hidden="true"><use href="#i-close" /></svg></button>
      </div>
    </header>

    <div class="dialog-body">
      <dl id="details-figures" class="details-kpi-grid" aria-live="off"></dl>

      <section class="details-section" aria-labelledby="details-speed-title">
        <div class="chart-header">
          <h3 id="details-speed-title">Speed history</h3>
          <dl id="details-speed-stats" class="chart-stats"></dl>
        </div>
        <svg id="details-graph" class="speed-graph" role="img" viewBox="0 0 640 165" preserveAspectRatio="none"
             aria-labelledby="details-graph-label">
          <title id="details-graph-label">Speed history for this download</title>
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

      <section class="details-section" aria-labelledby="details-meta-title">
        <h3 id="details-meta-title">Properties</h3>
        <dl id="details-info" class="details-info"></dl>
      </section>
    </div>

    <div class="dialog-actions details-footer">
      <div id="details-actions" class="job-actions"></div>
      <button id="details-close" class="quiet" type="button">Close</button>
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
