# Fetchpath

A download manager for Windows 11 that keeps your downloads in one list,
pauses and resumes them, survives restarts, and checks that what arrived is
exactly what you asked for. It can also save video and audio.

**Get started:** install it, then paste a link into the window. The
[user guide](docs/user/GUIDE.md) covers everything else.

## What it does

- **One list for everything.** Queue a single link or a whole batch, start
  downloads now or at a set time, and search and filter what you have.
- **Pause and resume.** A paused download carries on from where it stopped,
  even after a restart. If the file changed on the website in the meantime,
  Fetchpath starts again rather than joining two different files.
- **Never overwrites.** A file that's already there is never replaced.
- **Honest progress.** Percent and time left appear only when the website says
  how big the file is. Otherwise you see how much has arrived, not a guess.
- **Checksums.** Paste a publisher's SHA-256 and Fetchpath saves the file only
  if it matches. Every finished download shows the SHA-256 of what arrived.
- **Downloaded once, reused.** A file that matched its checksum is kept in a
  bounded cache, so the same file again comes from this computer, checked
  again, without the network.
- **Models and datasets from Hugging Face.** Paste a repository link and
  every file comes from one pinned commit, checked against the checksums
  Hugging Face states. Public repositories only for now.
- **Your own computers help each other.** Pair two PCs on the same network
  and a download with a checksum is taken from the other one first. Sharing
  is off until you turn it on.
- **Video and audio.** Choose a quality and save it, using `yt-dlp` and
  `ffmpeg`. Fetchpath sets both up for you from their official releases and
  checks them first.
- **From your browser.** Right-click a link in Chrome or Edge and choose
  **Send link to Fetchpath**. Downloads that need you to be signed in keep
  working.
- **Keyboard and screen reader friendly.** Every control has a name, and
  everything can be done from the keyboard.
- **Downloads keep going.** A background engine keeps the one list, so
  closing the window or a terminal never stops a download, and every way in
  sees the same list.
- **A command line and an interactive terminal.** `fetchpath download LINK`
  for scripts, the whole queue from the command line, and `fetchpath` on its
  own for a live terminal view with themes, keys and aliases of your own.
- **Rules.** Send downloads to a folder, pick a video quality or require a
  checksum by site, file type or size.
- **AI agents, on your terms.** Agents such as Claude Code can download
  through `fetchpath mcp` into the folders you grant; anything else waits for
  you to approve it. See [CLI.md](docs/user/CLI.md#ai-agents-mcp).

## Requirements

Windows 11 on a 64-bit Intel or AMD PC. ARM-based PCs aren't supported in
0.1.0.

## Installing

Download `Fetchpath_0.1.0_x64-setup.exe` and `SHA256SUMS.txt` from the
[release page](https://github.com/idcesares/fetchpath/releases/latest), check one against the other, and run the installer. It installs just for
you and doesn't need administrator rights. Then open Fetchpath from the Start
menu. The [user guide](docs/user/GUIDE.md#install) walks through each step.

Version 0.1.0 isn't code-signed, so **Windows SmartScreen warns when you run
the installer**. Once the checksum matches, choose **More info → Run anyway**.
Where **Smart App Control** is on, Windows blocks unsigned programs outright
and 0.1.0 can't be installed; see the [user guide](docs/user/GUIDE.md#install).

## Limitations

- HTTP and HTTPS links only. HTTP/2 is used where the server offers it.
  HTTP/3, FTP and SFTP aren't available in the app yet.
- Video and audio can't be paused, and no particular website is promised to
  work; that depends on `yt-dlp`.
- The browser extension is added in developer mode for now. Firefox isn't
  supported yet.
- Fetchpath doesn't claim to download faster than your browser does.
- Not code-signed, and x64 only.

See the [changelog](CHANGELOG.md) for what's in this version.

## Licence

Fetchpath is available under either the [MIT licence](LICENSE-MIT) or the
[Apache License 2.0](LICENSE-APACHE), at your option. Third-party components
are listed in
[THIRD-PARTY-NOTICES.md](apps/desktop/src-tauri/THIRD-PARTY-NOTICES.md).
`yt-dlp` and `ffmpeg` aren't part of Fetchpath and keep their own licences.

## Security

Report vulnerabilities privately; see [SECURITY.md](SECURITY.md).

## Contributing

Building, testing and the project's working rules are in
[CONTRIBUTING.md](CONTRIBUTING.md).
