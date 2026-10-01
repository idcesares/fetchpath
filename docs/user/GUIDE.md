# Fetchpath user guide

Fetchpath is a download manager for Windows 11. It keeps a queue of your
downloads, pauses and resumes them, survives restarts, and checks that what
arrived is exactly what you asked for. It can also save video and audio.

This guide covers version 0.1.0. You don't need to read it all: the first
two sections are enough to start. After that, use it as a reference.

- [Install](#install)
- [Your first download](#your-first-download)
- [The download list](#the-download-list)
- [Pause, resume and restarts](#pause-resume-and-restarts)
- [Several links at once](#several-links-at-once)
- [Starting later](#starting-later)
- [Checking a download against a checksum](#checking-a-download-against-a-checksum)
- [Video and audio](#video-and-audio)
- [Sending links from your browser](#sending-links-from-your-browser)
- [Settings](#settings)
- [Requests from AI agents](#requests-from-ai-agents)
- [The command line](#the-command-line)
- [Uninstalling](#uninstalling)
- [When something goes wrong](#when-something-goes-wrong)

## Install

**What you need:** Windows 11 on a 64-bit Intel or AMD PC. ARM PCs such as
Snapdragon laptops aren't supported in this version.

Fetchpath is free, open source software provided **as is**, without a warranty
or a promise of support. You choose what to download and are responsible for
using it lawfully and checking whether its sources and files are safe. The
[MIT](../../LICENSE-MIT) or [Apache 2.0](../../LICENSE-APACHE) licence, at your
option, sets the terms for using Fetchpath.

1. From the Fetchpath release page, download two files:
   `Fetchpath_0.1.0_x64-setup.exe` and `SHA256SUMS.txt`.
2. *Recommended:* check the installer arrived intact. In File Explorer, open
   the folder you saved them to, click the address bar, type `powershell` and
   press <kbd>Enter</kbd>. Then run:

   ```powershell
   (Get-FileHash .\Fetchpath_0.1.0_x64-setup.exe).Hash.ToLower()
   Get-Content .\SHA256SUMS.txt
   ```

   The first line printed must be the same as the long code at the start of
   the second. If they differ, delete both files and download them again.
3. Double-click `Fetchpath_0.1.0_x64-setup.exe` and follow the installer.
   In an interactive setup, you can choose whether to download the video and
   audio tools. You can also choose to open the bundled browser extension
   folder and see the Chrome or Edge setup steps. Both choices default to No;
   silent setup skips them. The browser still asks you to load the extension.
   Fetchpath installs just for you, doesn't ask for administrator rights,
   and goes into `%LOCALAPPDATA%\Fetchpath`. If your PC doesn't have the
   Microsoft Edge WebView2 Runtime (almost every Windows 11 PC does), the
   installer downloads it from Microsoft.
4. Open Fetchpath from the Start menu. A short welcome banner appears the
   first time; choose **Got it** when you've read it.

**Updating.** Run the new installer over the old one. Your download list,
history and settings are kept. If downloads are running, the installer stops
Fetchpath first; each download keeps what it has received and carries on from
there the next time Fetchpath starts.

**"Windows protected your PC."** Fetchpath 0.1.0 isn't signed with a
code-signing certificate, so Microsoft Defender SmartScreen doesn't recognise
it and warns when you run the installer. If you downloaded it from the
official release page and the checksum matched in step 2, choose
**More info**, then **Run anyway**. If you got the file anywhere else, don't
run it. The installer may also show the publisher as unknown, for the same
reason.

**"An App Control policy blocked this file."** This is Smart App Control, a
Windows 11 protection that is on for some new installations. It blocks
programs that are not signed, with no option to run them anyway, so the
unsigned 0.1.0 cannot be installed while it is on. You can see its state in
Windows Security → App & browser control → Smart App Control. Turning it off
is your decision to make; Fetchpath does not ask you to, and a signed build is
the real fix.

**"Allow Fetchpath on your network?"** Only if you use the command line's
paired-device sharing. The desktop app doesn't ask.

## Your first download

1. Copy a link to the file you want, for example from your browser.
2. Click anywhere in the Fetchpath window and press <kbd>Ctrl</kbd>+<kbd>V</kbd>.
   The **Add download** window opens with the link already filled in.
   You can also choose **Add download** at the top of the window.
3. Fetchpath suggests a name in your Downloads folder. Keep it, type another,
   or choose **Choose…** to pick a place.
4. Choose **Add to queue**.

The download appears in the list with its progress, speed and time left. When
it finishes, choose **Open folder** to see it in File Explorer.

Fetchpath **never overwrites a file**. If a file with that name is already
there, the download stops and asks you to choose a new name.

## The download list

Each row shows the file name, where it came from, and its state. While a file
downloads, the row also shows its progress, speed and time left.

- **Percent and time left appear only when the website says how big the
  file is.** When it doesn't, Fetchpath shows how much has arrived so far
  rather than guessing.
- The **SHA-256** shown on a finished download is a fingerprint of the file
  Fetchpath computed on your computer. It proves which bytes you have. It
  doesn't prove who published them. For that, compare it with a checksum the
  publisher lists (see [below](#checking-a-download-against-a-checksum)).
- Use the filters (**Active**, **Paused**, **Completed**, **Needs attention**…)
  and the search box (<kbd>Ctrl</kbd>+<kbd>F</kbd>) to find downloads.
- **Remove** takes a finished download off the list. The file stays on your
  computer.

The number at the top of the window shows how many downloads are active right
now.

## Pause, resume and restarts

**Pause** stops a file download and keeps what has already arrived.
**Resume** carries on from that point, even after you restart Fetchpath or
your computer, as long as the website still offers the same file. If the file
has changed on the website, Fetchpath starts again rather than joining two
different versions together.

**Pause all** and **Resume all** appear when there's more than one download to
act on.

Video and audio downloads can't be paused. You can cancel them and start again.

Downloads run in the background, so closing the window never stops them.
By default the window goes to the notification area (near the clock); click
the Fetchpath icon there to bring it back. **Quit Fetchpath** in that icon's
menu closes the window too, and downloads still finish. Fetchpath's
background part stops by itself about a minute after the last download ends
and every window is closed. To stop downloads, pause or cancel them first.

If Fetchpath's background part was stopped on purpose, for example with
`fetchpath engine stop`, the window says so and offers **Start Fetchpath**;
downloads carry on from where they were once you start it.

## Several links at once

Paste several links, one per line, into **Add download**. A preview lists
every file and where it'll be saved. The first file uses the name you choose;
the others go in the same folder under their own names, and no two are given
the same name.

## Starting later

Open **Advanced options** in **Add download** and set a **Start time**. The
download waits in the list as *Scheduled* and starts at that time, as long as
Fetchpath is running. If Fetchpath isn't running then, the download starts the
next time you open it. Fetchpath never wakes a sleeping computer.

**Start now** on a scheduled row starts it straight away.

## Checking a download against a checksum

Many publishers list a SHA-256 checksum next to their downloads. To have
Fetchpath check it:

1. In **Add download**, open **Advanced options**.
2. Paste the publisher's SHA-256 into **SHA-256 checksum**. Capital letters,
   spaces around it and a leading `sha256:` are all fine.
3. Add the download as usual.

If the file matches, the row says it **matches the checksum you entered**. If
it doesn't, Fetchpath **doesn't save it**. The row shows both checksums, with
**Edit checksum** and **Retry**. A mismatch usually means the checksum was
copied from the wrong line, or the file on the website has changed.

A match tells you the file is exactly the one that checksum describes. How far
to trust it depends on where you got the checksum.

This works for one file at a time, not for a batch.

Fetchpath keeps a copy of each file that matched its checksum. If you download
the same file again with the same checksum, it is copied from that copy and
checked again, without using the network, and the row says **Reused from this
computer's cache**. **Settings → Storage and sharing → Cache** shows how much it holds, sets its
size and clears it.

## Models and datasets from Hugging Face

Paste a Hugging Face link, such as `https://huggingface.co/owner/name`, into
**Add download**. Fetchpath looks the repository up and says which commit it
will download, how many files and how large, and how many it checks against
the checksum Hugging Face states. Choose the folder: the files go in a folder
named after the repository, with the repository's own layout. Every file
comes from the same commit, even if the repository changes while you
download. Only public repositories work for now.

## Video and audio

Fetchpath can save video and audio from pages that the free tool `yt-dlp`
supports, and lets you choose the quality. It needs two programs, `yt-dlp` and
`ffmpeg`, which don't come with Fetchpath.

**Set them up once:** choose the optional setup step in the installer, or open
**Settings → Integrations → Video and audio** and choose
**Download and set up**. Fetchpath downloads specific versions from their
publishers' official releases and installs them only if they match the
checksums recorded in this version of Fetchpath. It shows each program's
licence before you start. If you already have both programs, choose **I
already have them…** and pick the folder that contains them.

From a terminal, `fetchpath tools install` does the same after listing what
it will download and asking, and the interactive terminal offers it when you
paste a video link before the programs are set up.

**Then:**

1. Copy the video page's link and paste it into Fetchpath.
2. Fetchpath recognises it as video, checks the available qualities and picks
   the best one up to 1080p, named after the video. This takes a few seconds.
3. Choose another quality if you like. The list shows only formats `yt-dlp`
   confirmed, plus **Audio only**.
4. Choose **Add to queue**.

Fetchpath decides on its own whether a link is a file or a video. If it guesses
wrong, choose **File or batch** or **Video or audio** yourself; that choice
sticks for that link.

**Good to know:**

- Fetchpath doesn't promise that any particular website works. Websites
  change often, and whether a page can be saved depends on `yt-dlp`.
- Only save what you have the right to save. Respect the website's terms and
  the creator's rights.
- If a download fails with a sign-in or "expired" message, open the page in
  your browser again and send the link again.

## Sending links from your browser

The Fetchpath installer registers the local bridge for Chrome and Microsoft
Edge, and can open the extension folder with setup instructions. You add the
extension to your browser once:

The [browser extension privacy page](BROWSER-PRIVACY.md) explains what the
extension reads, sends to the local app, and keeps on your computer.

1. Open **Settings → Integrations → Browser extension**. It should say Fetchpath is
   connected to Chrome and Edge.
2. Choose **Copy Chrome address** or **Copy Edge address**, paste it into
   your browser's address bar, and press <kbd>Enter</kbd>.
3. Turn on **Developer mode** (a switch on that page).
4. Choose **Load unpacked**. In the folder picker, paste this into the
   address bar at the top and press <kbd>Enter</kbd>, then choose
   **Select Folder**:

   ```text
   %LOCALAPPDATA%\Fetchpath\browser-extension
   ```

   **Copy folder path** in Settings copies exactly this folder, and
   **Open extension folder** shows it in File Explorer.
5. Pin Fetchpath to the toolbar (the puzzle-piece icon, then the pin) and
   click it. It should say **Connected to Fetchpath**.

**Using it:**

- **Click the Fetchpath button** in the toolbar. It shows whether Fetchpath is
  connected, and **Send this page to Fetchpath** sends the page you're on,
  which is how you save a video from its page.
- **Right-click** a link and choose **Send link to Fetchpath**, a video and
  choose **Send this video to Fetchpath**, or an empty part of a page and
  choose **Send this page to Fetchpath**.
- Fetchpath opens (or comes to the front) with what you sent. A file starts
  downloading; a video page opens **Add download** with a quality chosen, so
  you only need to confirm.
- A file you send starts downloading even if the window can't open. A video
  page waits, however long, until you next open Fetchpath.

The first time you send from a website, the browser asks whether Fetchpath may
use that site; this lets it pass along your sign-in for that site only, so
downloads that need you to be signed in keep working. (Video pages are handed
over without your sign-in.)

**Automatic mode.** In the toolbar popup, turn on **Send downloads to Fetchpath
automatically** and allow the permissions the browser asks for. From then on,
downloads you start in the browser go to Fetchpath instead. If the native host
refuses a handoff, the extension tries to restart the browser download. It's
off until you turn it on.

**Why "Developer mode"?** Version 0.1.0 isn't listed in the Chrome Web Store
or Edge Add-ons yet, so the browser treats it as an extension you added
yourself. Chrome may remind you about developer-mode extensions when it
starts; that's expected. Firefox only allows signed add-ons, so Firefox isn't
supported in this version.

## Settings

Open Settings with the **Settings** button or <kbd>Ctrl</kbd>+<kbd>,</kbd>.
Choose General, Downloads, Integrations, Agents and rules, or Storage and
sharing. Changes save straight away. The footer links to the Fetchpath project
on GitHub.

| Setting | What it does |
| --- | --- |
| Downloads at the same time | 1 to 8. More at once isn't always faster. |
| Default save folder | Where new downloads and links from your browser go. |
| Retry connection problems automatically | Retries only network and server trouble, up to the number of attempts you choose. Problems that need you, such as a file that already exists or an expired sign-in, always wait for you. |
| Video and audio | Sets up `yt-dlp` and `ffmpeg`. |
| Browser extension | Shows whether the browser connection is installed, and helps you add the extension. |
| Rules | Where a new download goes and how, by the site it comes from, its type or its size: a folder, a video quality, a required checksum or a limit on connections. Rules are tried in order and the first that matches decides. **Test a link** says which rule decides and why. |
| Cache | Copies of downloads that matched a checksum, so the same file downloaded again doesn't use the network. Shows how much it holds, sets how much it may keep (the oldest copies go first), and **Clear cache**. Clearing it never touches your saved files. |
| Paired computers | Lets your own computers give each other files downloaded with a checksum from a link that needed no sign-in. **Show a pairing code** on one, then on the other choose **Pair with a computer that shows a code** and enter its address and code; each shows the other's fingerprint so you can compare them. Sharing stays off until you turn it on, and **Remove** unpairs a computer at once. When a paired computer on the same network is sharing, a download with a checksum is taken from it first, checked, and its row says **From your paired computer**. |
| AI agents | Which AI agents (such as Claude Code) may download without asking: the folders each may save into, its largest download and downloads an hour, and **Revoke access**. |
| Keep running in the notification area | Whether closing the window keeps its icon in the notification area. Downloads continue either way. |
| Ask before removing a finished download | A confirmation before **Remove**. |
| Power mode | Adds a statistics panel and per-download details. Nothing else moves. |
| Appearance | Match Windows, Light or Dark. |

Everything works with the keyboard alone. Press **Keyboard help** for the
shortcuts.

## Requests from AI agents

An AI agent connected through `fetchpath mcp` (see [CLI.md](CLI.md#ai-agents-mcp))
downloads into the folders you give it in Settings without asking. Anything
else it asks for appears in the list as **Waiting for approval**, with the
agent's name and why it asks; Fetchpath announces it, and it shows under
**Needs attention**. **Approve** lets it download; **Deny** cancels it and the
agent is told. Nothing is downloaded while it waits. Taking a folder away or
revoking an agent in Settings sends its unfinished downloads there back to
waiting.

## Download speed

Speed depends on the website and your connection. More connections cannot
remove a limit shared by the whole download. Fetchpath makes no blanket
promise to beat your browser. The [development comparisons](../development/ADAPTIVE-HTTP.md#tool-comparisons-fp-087-30-september-2026)
show the full engine against curl, aria2, wget2 and Chrome, with the settings,
connections used and slower cases. They do not describe every website or
promise the same result from the released installer.

## The command line

The installer also adds a `fetchpath` command. Open a **new** terminal and run:

```powershell
fetchpath download https://example.com/file.zip
```

Run `fetchpath` on its own for the interactive terminal: your downloads
above a prompt where you paste links, and `/` commands to pause, resume,
cancel or change them. Both work on the same list as the window, so a download added in
one shows in the others.

**Rules** (in Settings, or `fetchpath rules`) apply however a download is
added: from this window, your browser, the command line or an AI agent. In
**Add download**, the rule that decides is named under the file name with its
reason, and its folder is proposed; a folder you choose yourself always wins.
A link from your browser that a rule requires a checksum for opens in **Add
download** so you can paste it.

See [CLI.md](CLI.md) for everything it can do, including connecting an AI
agent.

## Uninstalling

Use **Settings → Apps → Installed apps** in Windows, find Fetchpath, and
choose **Uninstall**. On the uninstaller's confirmation page, tick **Delete the
application data** only if you also want your download list, history,
settings, media tools and cache removed. Leave it unticked, as it starts, if you're
reinstalling.

**Files you downloaded are never removed**, wherever you saved them.
If downloads are still running, the uninstaller stops Fetchpath first.

The uninstaller also removes the browser connection, the `fetchpath`
command from your PATH and, if you turned it on, starting at sign-in. Remove the extension from your browser yourself on its
extensions page.

## When something goes wrong

| What you see | What to do |
| --- | --- |
| **SmartScreen warning** when installing | See [Install](#install). |
| **"saved by a newer version of Fetchpath"** | You opened an older Fetchpath after a newer one. Your list is shown but not changed, and nothing downloads. Install the newer version again to carry on. |
| **"already exists"** | Fetchpath never overwrites. Choose **Choose new path**, or rename or move the old file. |
| **"Edit link" / "Refresh source"** | The link has expired or stopped working. Get a fresh link from the website and paste it. |
| **"Send again from browser"** | The download needed your browser sign-in, and it has expired. Send the link from your browser again. |
| **Connection or server errors** | Choose **Retry**. Turn on automatic retry in Settings to have Fetchpath retry these for you. |
| **Checksum doesn't match** | See [Checking a download against a checksum](#checking-a-download-against-a-checksum). |
| **"Media tools are not set up yet"** | Open **Settings → Integrations → Video and audio** and choose **Download and set up**. |
| **Send link to Fetchpath is missing** | Check the extension is added and turned on in your browser's extensions page, and that **Settings → Integrations → Browser extension** shows a check mark. |
| **The toolbar popup says "Fetchpath isn't answering"** | Fetchpath must be installed with its installer on this computer. Reinstall it, then close and reopen the popup. |
| **The `fetchpath` command isn't found** | Open a new terminal after installing. If it still isn't found, your PATH was too long to change safely; add `%LOCALAPPDATA%\Fetchpath` to it yourself. |
| **A download doesn't resume after a restart** | The website may not support resuming, or the file changed. Fetchpath starts again rather than joining two different files. |

Your download list and settings live in `%APPDATA%\app.fetchpath.desktop`.
If the settings file is ever damaged, Fetchpath starts from its defaults and
tells you so in Settings.
