# Fetchpath desktop UX contract

## Product promise

Fetchpath is a Windows-first download manager that makes a link feel safe and simple: paste it, choose what matters, and see an honest, controllable result. It offers familiar queue controls for ordinary users while keeping diagnostics and tuning available in **Power mode**. The desktop client never implies a download has started, a format is available, or a source is reachable until the underlying service confirms it.

## Information hierarchy

1. **Queue** is the home and primary work surface: status, progress, remaining time, destination, and one clear next action per item.
2. **Add download** is the primary command, always in the app bar. It opens a dialog that holds the whole composer (links, destination, media quality, start time, checksum, batch preview). Pasting a link anywhere outside a text field opens it pre-filled, and `Ctrl+N` or `Ctrl+L` opens it from anywhere. Pasted links are always reviewed before starting. `Esc` closes it and keeps the draft; **Cancel** clears it.

   *Decision, 23 September 2026 (FP-035):* this replaces the earlier inline composer. The inline composer was chosen so the queue stayed visible while links were reviewed, but on the shipped build the hero, welcome card and composer together pushed the queue below the fold even in a maximized 2560×1550 window, so the queue was never visible in practice. A dialog over a full-window queue serves the same "several links in a row" need through paste-to-add and batch input.
3. **Item actions** are contextual: pause/resume, open destination when available, remove/cancel. Destructive actions require a confirmation only when completed files would be deleted.
4. **Filters** reduce a long queue without moving items elsewhere: All, Active, Completed, Failed.
5. **Power mode** exposes diagnostic and transfer details inline, plus a session statistics panel above the queue. It is off by default, it is turned on in Settings, and it is strictly additive: nothing it shows replaces or moves a control that is available without it.
6. **Settings** holds the choices a power user wants and an ordinary user never has to open: concurrency, the default save folder, automatic retry, media tool setup, window behaviour and appearance. Every value is bounded, and an unreadable settings file falls back to documented defaults rather than failing launch.

## Core journeys

### Download a file

1. Select **Add link** (`Ctrl+L`), paste a URL, optionally name it, and select a destination.
2. Choose **Add to queue**. The item enters `Queued`, then `Connecting`, then `Downloading` only after confirmation from the download engine.
3. Pause/resume is immediate in the interface and is reconciled with the engine. Completion shows the saved location and an **Open folder** action.
4. Optionally, under **Advanced options**, paste the SHA-256 a publisher lists for the file. Fetchpath then saves the file only if it matches. Surrounding spaces, either case and a `sha256:` prefix are accepted. A checksum describes one file, so it is refused on a batch, and it is not offered for video or audio, which are assembled locally.

### Save video or audio

1. Add a supported media URL. Fetchpath first inspects the source and presents only engine-confirmed streams.
2. Choose Video or Audio, a quality/format, and a filename. The default is the best compatible quality with its size estimate.
3. Start or queue the selected variant. If inspection fails, retain the link, explain why, and offer retry/copy details; never invent a quality list.

### Resolve a problem

1. A failed item stays visible with a human-readable reason and next action (retry, edit link, sign in where supported, or copy diagnostics).
2. Network/transient failures retry only when enabled in settings; the queue shows the next retry time.
3. Authentication, source expiry, disk-space, and permission failures require an explicit user action.

## States and language

| State | Plain-language presentation | Available action |
| --- | --- | --- |
| Scheduled | “Scheduled for &lt;time&gt;” | Start now, pause, cancel |
| Queued | “Queued” | Pause, cancel |
| Downloading | Percent, received of total, speed, time remaining | Pause, cancel |
| Paused | “Paused”, with the bytes already verified | Resume, cancel |
| Cancelling | “Cancelling…” | none while it unwinds |
| Complete | “Complete”, with the observed SHA-256; when a checksum was supplied, “matches the checksum you entered” | Open folder, copy path, remove from list |
| Needs attention | Specific reason plus one next action | Retry, edit link, choose new path, set up media tools |
| Checksum mismatch | “Doesn't match the checksum you entered, so nothing was saved”, with the expected and received values | Edit checksum, retry |
| Link needed | The private source was not saved | Paste a refreshed link, or re-send from the browser |

**Progress is only ever reported from engine-confirmed numbers.** A source that
states no length produces no percentage and no remaining time; the row shows the
bytes received and says the total is unknown, and the bar stays indeterminate.
Filling a bar from the bytes received so far would read as complete from the
first chunk onward.

**Pause is offered only where it can be honoured.** A file download pauses to its
retained checkpoint and resumes from that offset. A media download has no such
checkpoint, so pause is not offered on those rows rather than offered and then
refused.

Offline is not yet a modelled state; a connection failure appears as a transport
failure with a retry, and automatic retry is the setting that governs it.

A checksum match means the bytes are exactly the ones the checksum describes. It is never described as proof of who published the file: that depends on where the checksum came from, and the interface says so. A mismatch is never retried automatically, because either the checksum or the source is wrong and a person has to decide which.

Avoid “success” until the file is safely written and verified by the engine. Avoid “unsupported” without naming the limitation where known.

## Accessibility and Windows behavior

- Meet keyboard-first operation: `Ctrl+N`/`Ctrl+L` opens Add download, `Ctrl+F` searches, `Esc` closes an unsubmitted dialog, `Tab` follows visual order, and dialog focus returns to its trigger, or to Add download when the trigger has gone. The two live regions move into whichever modal dialog is on top, because a modal makes the page behind it inert and an inert live region is never announced.
- Use native buttons, form labels, visible focus rings, status text, and `aria-live` announcements for changes such as pause, resume, and queue additions.
- Maintain 4.5:1 text contrast, do not rely on color alone for status, support 200% text scaling, and preserve useful layout down to a 900 px-wide window.
- Honor system reduced motion. Animation conveys progress only and must not be necessary to understand state.
- Respect Windows conventions: standard title-bar commands, predictable right-click/context actions, default Downloads destination, and paths that can be copied.

## Prototype boundary

The accompanying prototype is a visual interaction model only. It simulates source inspection, quality choices, queue progress, pause/resume, filters, and diagnostics locally. It has no network access, does not inspect URLs, and does not create, change, or download files.
