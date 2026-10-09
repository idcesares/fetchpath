# Security

## Reporting a vulnerability

Report privately through
[GitHub's private vulnerability reporting](https://github.com/idcesares/fetchpath/security/advisories/new),
not in a public issue. Say what you found, the version, and the steps to
reproduce it. You should hear back within a week; a fix and an advisory
follow once the report is confirmed.

Only the latest release receives security fixes.

## In scope

- A download saved as something other than what was asked for: a checksum
  mismatch accepted, a resume that joins two different files, or a file
  replaced or written outside its folder.
- The engine's pipe and its secret, the browser native host, and the
  installer's handling of the engine, PATH and user data.
- Instance identity across reconnection, and guided agent-host configuration
  ownership, recovery and removal without changing unrelated host settings.
- The agent boundary: `fetchpath mcp` acting beyond the folders, limits and
  approvals the person granted, reaching credentials, settings, rules or
  sharing, or page text steering an agent unmarked.
- Paired computers: pairing without the code, sharing without consent,
  serving files that were not shared, or content accepted without its
  checksum.
- The loopback web UI listener: reachable while it is off, from another
  computer or from a page on another site (host, origin or cookie checks), a
  sign-in link used twice or after a minute, a browser session doing more than
  viewing and submitting downloads, or a session or link written to disk or
  logs in readable form.
- Media helpers run without the pinned digest matching.
- Torrent metadata that escapes the chosen folder, peer discovery (for an agent, browser capture or speed profile) or uploading
  without the person's consent, or a torrent marked complete before its files
  are checked and published safely.
- HTTP download redirects exposing source credentials or cookies to another origin.
- Secrets or private URLs written to logs.

## Dependency validation

Release checks include dependency advisories and secret scanning. These checks
complement boundary tests and review; they do not guarantee that no flaw exists.
The release record documents known advisories and their assessed applicability.
SFTP RSA private-key authentication is disabled while the upstream RSA timing
advisory has no patched version. Password and non-RSA key authentication remain
available; RSA public server-key verification is unaffected.

## Out of scope

- A malicious program already running as the same Windows user; Fetchpath
  does not defend against it.
- SmartScreen and Smart App Control warnings about the unsigned installer.
- Whether a particular website works with `yt-dlp`, and flaws in `yt-dlp`
  or `ffmpeg` themselves; report those upstream.
