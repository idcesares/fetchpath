# Interactive terminal

FP-059 (foundation); FP-060 to FP-063 extend it. Source: `apps/cli/src/tui`.

## What exists

`fetchpath` with no arguments, in a terminal, opens the interactive mode as a
client of the engine (started if none is running). Without a terminal, or
with `--json`, it prints help and exits 2.

- **Inline mode.** A ratatui inline viewport (crossterm backend) at the bottom
  of normal scrollback: a live panel of active, needing-attention, paused,
  queued and scheduled jobs (at most eight rows or a third of the window, then
  "N more"), a summary rule, the prompt and a hint line. Finished jobs print
  one-line receipts into scrollback; so do warnings and recorded problems. The
  viewport's height follows the panel by reopening the viewport where it
  began, because ratatui's inline height is fixed.
- **Look first, then confirm.** A pasted link (or `/add`) is not queued at
  once. `review.rs` asks the engine (`InspectLink`, then `InspectMedia` for a
  media page or a web page) on its own thread while a card shows a spinner.
  The card then shows, for a video, the title, length and formats (tallest
  first, the best up to 1080p preselected as the desktop does, or the
  `--quality` given), chosen with arrows or a digit; for a file, its name
  (from `Content-Disposition`, made safe), type, size and whether it resumes;
  for a web page, a warning that it is not a file. Every card shows where the
  file will be saved. Enter queues exactly what the card shows; Esc cancels.
  A video page whose formats cannot be read (media tools not set up) cannot
  be started from its card, so a page is never saved in a video's place.
  Several links are carded one after another. Plain mode prints the card as
  lines and reads the answer (Enter, a number, or `no`).
- **Progress.** Each panel row has a bar sized to the window (colored by
  state; a sliding block when the size is unknown), the percent, speed and
  time left, and a spinner while moving. Narrow windows drop the bar first.
- **Prompt.** A bare line is `/add` with its words; `/` commands mirror the
  command line (`add queue show pause resume cancel retry rm history settings
  engine help quit`) through the same `queue.rs` helpers, so wording and
  results match. Tab completes command names, jobs (by number, id or name,
  inserting the short id), setting names, and folders after `--to` (recent
  download folders, then folders on disk). Up/Down recall this session's
  lines; Esc clears; Ctrl+C clears, then leaves on an empty line. Leaving never
  stops a download.
- **Plain mode.** `--plain`, or automatic under `NO_COLOR`, `TERM=dumb` or
  Windows' screen-reader flag: append-only lines, input read with the
  console's own line editing, one line per durable event and a receipt per
  finish.
- **Glyphs.** Unicode under Windows Terminal (`WT_SESSION`) or a terminal that
  sets `TERM_PROGRAM`; ASCII elsewhere, because the classic console's default
  fonts lack most symbols.

The queue view (`live.rs`) subscribes at the engine's current cursor, then
merges a full listing; each job's `seq` against a snapshot's `last_seq` skips
events already reflected, so nothing is lost or applied twice. After a state,
error, policy or publication event it fetches the job for fields no event
carries. A failure prints its receipt only from that snapshot, since the
event alone cannot tell a final failure from one with a retry due. A lost
stream reconnects (starting the engine if needed) and resynchronizes.

## Decision: in-house prompt, not a line-editor crate

Probed 26 September 2026. Line editors (rustyline, reedline) own the cursor
and screen while reading a line, which cannot coexist with a panel redrawn
above the prompt several times a second. The prompt is a small editor
(`prompt.rs`) drawn inside the viewport. Windows reports key releases, so only
presses and repeats type; Windows consoles deliver no bracketed paste, so a
pasted line arrives as keys, and newlines become spaces or submit the line.

## Verification

- Render snapshot of the panel from a queue stream recorded from a real engine
  (`apps/cli/tests/fixtures/queue-stream.jsonl`, paths anonymized); receipt,
  plain-mode, replay-without-double-apply, panel order, prompt editing and
  completion unit tests (`cargo test -p fetchpath --bin fetchpath tui`).
- `without_a_terminal_the_interactive_mode_prints_help_and_exits_2` in
  `apps/cli/tests/queue.rs`.
- Cards: format order and the 1080p default, preselected `--quality`, safe
  server file names, type labels, the rendered media card, typed plain-mode
  answers (`tui::review`, `tui::view`, `tui::plain` tests); `inspect_link`
  against local servers (`fetchpath-http`); and `InspectLink` through the
  engine, including an agent refused a link with user info
  (`fetchpath-session/tests/policy.rs`).
- Headless ConPTY walkthrough of the cards, 26 September 2026: a file card
  (name, type, 23.8 MiB, cannot resume, folder) then Enter and an animated
  bar to the saved receipt; a local HTML page carded as a web page and
  cancelled; a YouTube link with media tools missing carded as a video that
  cannot start; and, with the pinned `yt-dlp` (SHA-256 matching the pin) in
  a scratch tools folder, the real YouTube page listed nine formats with
  1080p preselected, moved with the arrow keys and cancelled. The same run
  on the previous build reproduced the reported bug: the YouTube link saved
  a 1.1 MiB page named `watch`.
- Keyboard-only walkthrough in a Windows pseudo-console (ConPTY, the layer
  under both Windows Terminal and the console host), driven headlessly and
  read back through a VT emulator, 26 September 2026: the engine was started
  on demand; a link queued and showed live progress; a 404 printed its
  receipt; Tab completed `/pa` and a job name; pause, resume, history recall,
  `/queue`, a resize to 70×20, the saved receipt, and Ctrl+C leaving with
  exit 0. Plain mode ran the same script and left with `/quit`. That
  walkthrough found and fixed a real bug: the pipe treats a zero read wait as
  "do not read", so the view now polls with at least 1 ms.

## Limitations

- A walkthrough by a person in Windows Terminal and the classic console host
  windows is still owed (the task's recorded manual check); the headless run
  exercises the same console layer but not either host's rendering or fonts.
- Narrowing the window clears the visible screen (ratatui's inline behavior on
  a horizontal shrink); scrollback above it is kept.
- Commands other than looking at links run synchronously; none of them
  waits on a site.
- The media card lists formats without sizes: the helper's inspection does
  not report them. Video downloads still need the media tools, set up once
  from the desktop's Settings (or `FETCHPATH_MEDIA_TOOLS_DIR`); the terminal
  has no setup flow of its own yet.
- `fetchpath add LINK` from scripts still queues a video page as a file
  unless `--quality` is given; only the interactive terminal looks first.
- Prompt history is kept for the session only; saving it (with query strings
  and user info stripped) is FP-063. Rules preview on paste is FP-064; media,
  batch, conflict, checksum and approval prompts are FP-061; themes and keys
  are FP-062; the dashboard is FP-060.
