# Fetchpath desktop UX contract

## Product promise

Fetchpath is a Windows-first download manager that makes a link feel safe and simple: paste it, choose what matters, and see an honest, controllable result. It offers familiar queue controls for ordinary users while keeping diagnostics and tuning available in **Power mode**. The desktop client never implies a download has started, a format is available, or a source is reachable until the underlying service confirms it.

## Information hierarchy

1. **Queue** is the home and primary work surface: status, progress, remaining time, destination, and one clear next action per item.
2. **Add link** is the primary command. It opens a focused dialog; pasted links are reviewed before starting.
3. **Item actions** are contextual: pause/resume, open destination when available, remove/cancel. Destructive actions require a confirmation only when completed files would be deleted.
4. **Filters** reduce a long queue without moving items elsewhere: All, Active, Completed, Failed.
5. **Power mode** exposes diagnostic and transfer details inline. It does not duplicate the normal workflow or make basic controls disappear.

## Core journeys

### Download a file

1. Select **Add link** (`Ctrl+L`), paste a URL, optionally name it, and select a destination.
2. Choose **Add to queue**. The item enters `Queued`, then `Connecting`, then `Downloading` only after confirmation from the download engine.
3. Pause/resume is immediate in the interface and is reconciled with the engine. Completion shows the saved location and an **Open folder** action.

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
| Queued | “Waiting for available connection” | Start now, remove |
| Connecting | “Checking source…” | Cancel |
| Downloading | Progress, speed, time remaining | Pause |
| Paused | “Paused” | Resume, remove |
| Completed | “Saved to Downloads” | Open folder, remove from list |
| Failed | Specific reason plus recovery | Retry, edit, copy details |
| Offline | “You’re offline. Downloads will resume when connected.” | Retry when online |

Avoid “success” until the file is safely written and verified by the engine. Avoid “unsupported” without naming the limitation where known.

## Accessibility and Windows behavior

- Meet keyboard-first operation: `Ctrl+L` opens Add link, `Esc` closes an unsubmitted dialog, `Tab` follows visual order, and dialog focus returns to its trigger.
- Use native buttons, form labels, visible focus rings, status text, and `aria-live` announcements for changes such as pause, resume, and queue additions.
- Maintain 4.5:1 text contrast, do not rely on color alone for status, support 200% text scaling, and preserve useful layout down to a 900 px-wide window.
- Honor system reduced motion. Animation conveys progress only and must not be necessary to understand state.
- Respect Windows conventions: standard title-bar commands, predictable right-click/context actions, default Downloads destination, and paths that can be copied.

## Prototype boundary

The accompanying prototype is a visual interaction model only. It simulates source inspection, quality choices, queue progress, pause/resume, filters, and diagnostics locally. It has no network access, does not inspect URLs, and does not create, change, or download files.
