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
- **Third-party tools.** Video and audio need `yt-dlp` and `ffmpeg`, which
  Fetchpath does not ship. At start a background check (`tools::status`)
  prints a note when they are missing. A video card for a page whose formats
  failed with `media.helper_unavailable` lists the two programs and offers
  **I**; so does a web-page card, since the page may hold a video. **I** or
  `/tools install` opens a setup card that shows each program's version,
  publisher and licence and the destination folder before anything is
  downloaded; Enter runs the desktop's installer (`fetchpath_media::setup`,
  pinned SHA-256) on its own thread with a bar per download, Esc cancels it,
  and afterwards the engine is pointed at the folder and the waiting video is
  looked at again. `fetchpath tools [install [--yes] | use FOLDER]` does the
  same from the command line and refuses to install without a terminal
  unless `--yes` is given.
- **Choosing instead of typing.** Typing `/` lists the matching commands
  under the prompt (eight at a time, following the selection); arrows or the
  wheel move, Tab completes, Enter or a click runs one (`/add` gets its link
  typed after it). A generic `menu.rs` list backs `/settings`
  (`settings_menu.rs`: switches flip, choices cycle, numbers take ←/→ within
  the engine's limits, folders open an edit line with folder completion,
  video tools open the setup card; each change is one `UpdateSettings` and
  the row shows what the engine kept) and `/queue` (`jobs_menu.rs`: jobs in
  progress then recent finished ones, live progress, then the actions that
  make sense for the chosen job's state). Job commands given no job open the
  same list filtered to the jobs they apply to. Plain mode keeps typed
  commands.
- **Show in folder.** `reveal.rs`: a saved file opens in File Explorer
  selected (`SHOpenFolderAndSelectItems` on the file's ID list, so spaces
  and any characters are safe); a file not written yet, or moved away, opens
  its folder with `explorer.exe FOLDER`, saying which. Offered first among a
  finished job's actions, as `/folder [JOB]` (a picker without a job) and
  `fetchpath folder JOB`. Verified 26 September 2026 by revealing a scratch
  download and reading Explorer's selection back through
  `Shell.Application` (the file name), then closing only that window.
- **Mouse.** Captured only while a list, menu or card is showing, so the
  terminal's own scrollback, wheel and text selection work at the prompt.
  Each frame records which screen row holds which item (`view::Drawn`); a
  left click chooses, the wheel moves, a right click closes a menu. Video
  cards take clicks on formats.
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
- Menus and mouse: `tui::menu`, `tui::jobs_menu` and `tui::view` tests (key
  and click selection, headings skipped, a rendered menu and command list with
  their click targets, the actions offered per state). A headless ConPTY run
  sending SGR mouse sequences, 26 September 2026: `/` listed the commands and
  `/se` narrowed them to `/settings`; Enter opened the menu; → raised
  downloads at the same time to 4 (saved); clicks switched automatic retries
  off and cycled the appearance after the wheel scrolled to it; `/queue`
  showed the running job with live progress, a click showed its actions and
  a click on Pause paused it; `/resume` alone asked which job and Enter
  resumed it; a click on `/queue` in the command list ran it.
- Headless ConPTY walkthrough of the tools setup, 26 September 2026, in a
  scratch data folder: the startup note; a YouTube card naming yt-dlp
  2026.08.19 (Unlicense) and ffmpeg 9.0.2 (GPL-3.0-or-later); **I** showed
  the setup card; Enter downloaded 17.0 MiB and 109.5 MiB with bars, both
  checked against their pins; the tools were reported ready and the same
  video's card came back with ten formats. `fetchpath add … --quality 360p
  --wait` then saved the 28.8 MiB video. `tools` and `tools install`
  without a terminal are covered in `apps/cli/tests/queue.rs`.
- Keyboard-only walkthrough in a Windows pseudo-console (ConPTY, the layer
  under both Windows Terminal and the console host), driven headlessly and
  read back through a VT emulator, 26 September 2026: the engine was started
  on demand; a link queued and showed live progress; a 404 printed its
  receipt; Tab completed `/pa` and a job name; pause, resume, history recall,
  `/queue`, a resize to 70×20, the saved receipt, and Ctrl+C leaving with
  exit 0. Plain mode ran the same script and left with `/quit`. That
  walkthrough found and fixed a real bug: the pipe treats a zero read wait as
  "do not read", so the view now polls with at least 1 ms.

- Strong review (FP-073), 26 September 2026, of 2b8dfff, 9779b2a and
  07e7d49; findings and fixes are in the commit messages. Regression tests:
  a redirect to FTP is not followed (`fetchpath-http` `link`), a
  right-to-left override cannot disguise an extension
  (`download::tests`), the size cap (`setup::tests`); the ignored real
  guided install passed again (193 s).

- Keyboard and mouse walkthrough by the person, 26 September 2026, in
  Windows Terminal and in the classic console host (`conhost`), release
  build at 07e7d49: a file link carded and downloaded with a moving bar;
  `/` then `/settings` chosen with the arrows, a setting changed, Esc; `/queue`
  then Show in folder opened File Explorer on the file; a YouTube link set up
  the video tools with I and offered its qualities; menus answered the
  mouse; Ctrl+C left. Everything worked in both hosts, with nothing garbled.

## Limitations

- `InspectLink` lets an agent read the headers of any HTTP(S) address the
  engine can reach, local network included; that is no more than a job it
  may already create. Redirects stay on HTTP(S), up to libcurl's 30.
- A helper already in the tools folder is used as found, not re-checked
  against its pin, so a newer pin does not replace it; a desktop and a
  terminal installing at the same moment are not serialized.
- Narrowing the window clears the visible screen (ratatui's inline behavior on
  a horizontal shrink); scrollback above it is kept.
- Commands other than looking at links run synchronously; none of them
  waits on a site.
- The media card lists formats without sizes: the helper's inspection does
  not report them.
- `fetchpath add LINK` from scripts still queues a video page as a file
  unless `--quality` is given; only the interactive terminal looks first.
- Prompt history is kept for the session only; saving it (with query strings
  and user info stripped) is FP-063. Rules preview on paste is FP-064; batch,
  conflict, checksum and approval prompts are FP-061; themes and keys
  are FP-062; the dashboard is FP-060.
