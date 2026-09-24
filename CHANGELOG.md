# Changelog

## 0.1.0 (unreleased)

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
  link starts Fetchpath if it is closed.
- Settings for concurrency, default folder, automatic retry of connection
  problems, window behaviour and appearance, plus an optional Power mode.
- Full keyboard operation and screen reader support.

### Command line

- `fetchpath download` with folder destinations, progress, `--sha256`,
  `--json` and documented exit codes, installed and added to your PATH. It
  needs no Visual C++ redistributable.

### Installer

- Per-user install with no administrator rights; upgrades keep your list.
- Uninstall asks before removing your list and settings, and never removes
  downloaded files. Your PATH is restored exactly as it was.

### Known limitations

- x64 only; ARM64 isn't supported.
- Not code-signed; SmartScreen warns on first run.
- HTTP and HTTPS only in the app. HTTP/3 isn't available in this build.
- Video and audio downloads can't be paused.
- The browser extension loads in developer mode; Firefox isn't supported.
- No speed claim is made.
