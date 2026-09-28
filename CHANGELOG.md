# Changelog

## 0.1.0 (28 September 2026)

The first public release of Fetchpath, for Windows 11 on x64.

### Desktop app

- A download list with search, filters, batch adding, scheduled start times,
  and recovery after a restart.
- Pause and resume for file downloads, including across restarts, with a
  fresh start instead of a mixed file when the source has changed.
- Progress, speed and time left from what the source actually states.
- Optional SHA-256 check against a publisher's checksum; a mismatch saves
  nothing.
- Video and audio downloads with quality selection, using `yt-dlp` and
  `ffmpeg`, which Fetchpath sets up from pinned, checksum-verified releases.
- Add download tells a file from a video on its own; a switch overrides it.
- A details window per download with its speed over time, which parts of the
  file have arrived and, for a segmented transfer, each range in flight.
- Browser capture from Chrome and Edge through the included extension, with a
  toolbar button that shows the connection and sends the current page; a sent
  link reaches the list even with the window closed.
- Settings for concurrency, default folder, automatic retry of connection
  problems, AI agents' access, window behaviour and appearance, plus an
  optional Power mode.
- Full keyboard operation and screen reader support.

### Engine

- One per-user background engine, `fetchpath engine`, owns the list,
  history, settings and rules. It starts when a client needs it and stops
  about a minute after the last download and client finish. The desktop,
  command line, terminal, browser connection and agents are all its clients,
  so closing any of them never stops a download.
- A list written by a newer Fetchpath is kept rather than discarded.

### Command line and terminal

- `fetchpath download` with folder destinations, progress, `--sha256`,
  `--json` and documented exit codes, installed and added to your PATH. It
  needs no Visual C++ redistributable.
- The whole queue from the command line: `add`, `batch`, `ls`, `show`,
  `pause`, `resume`, `cancel`, `retry`, `rm`, `watch`, `history`,
  `inspect`, `settings`, `folder` and `engine`, each with `--json`.
- `fetchpath` on its own opens the interactive terminal: a live panel above
  a prompt, a card before each download starts, batches, name conflicts,
  checksums, a full-screen dashboard with speed graph and connections, and a
  plain mode for screen readers.
- Themes (including high contrast), glyphs, density, keys and aliases in
  `cli.toml`.
- `fetchpath tools` sets up the video and audio programs from the terminal.

### Cache, Hugging Face and paired computers

- Files that matched a checksum are kept in a bounded cache; the same file
  again is copied from it and checked, without the network. Settings and
  the terminal show its size, set its limit and clear it.
- Hugging Face links download a public repository's files from one pinned
  commit, each checked against the checksum Hugging Face states, from the
  window, the command line and agents.
- Pair your own computers with a one-time code and compare fingerprints.
  When a paired computer on the same network is sharing, a download with a
  checksum asks it first and checks what it gives. Sharing is off by
  default and never covers links that needed a sign-in.

### Rules

- Rules by site, file type or size choose the folder, video quality, a
  required checksum and the connection limit, for downloads from the window,
  the browser, the command line and agents. Settings lists, adds, removes
  and tests them; Add download names the rule that decides and proposes its
  folder; `fetchpath rules test` says which rule decides and why.

### AI agents

- `fetchpath mcp` serves Fetchpath to agent hosts over the Model Context
  Protocol. Agents download into the folders you grant, within a size and
  hourly limit; anything else waits for **Approve** or **Deny** in the
  window, the terminal or `fetchpath approve`. Agents never send
  credentials, replace files or change settings, rules or sharing, and
  text from web pages reaches them marked as untrusted.
- Settings and `fetchpath agents` show and change each agent's access.

### Installer

- Per-user install with no administrator rights; upgrades keep your list.
- Setup stops the engine safely before an upgrade or uninstall and keeps it
  from restarting until setup finishes.
- Uninstall asks before removing your list and settings, and never removes
  downloaded files. Your PATH is restored exactly as it was.

### Known limitations

- x64 only; ARM64 isn't supported.
- Not code-signed; SmartScreen warns on first run, and where Smart App
  Control is on, Windows blocks the installer.
- HTTP and HTTPS only in the app. HTTP/3 isn't available in this build.
- Video and audio downloads can't be paused.
- The browser extension loads in developer mode; Firefox isn't supported.
- The protection against agents assumes no malicious program already runs
  as you.
- Paired computers find each other only on one local network.
- Hugging Face downloads work with public repositories only.
- No speed claim is made.
