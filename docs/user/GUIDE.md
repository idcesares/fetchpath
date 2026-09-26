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
- [The command line](#the-command-line)
- [Uninstalling](#uninstalling)
- [When something goes wrong](#when-something-goes-wrong)

## Install

**What you need:** Windows 11 on a 64-bit Intel or AMD PC. ARM PCs such as
Snapdragon laptops aren't supported in this version.

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

## Video and audio

Fetchpath can save video and audio from pages that the free tool `yt-dlp`
supports, and lets you choose the quality. It needs two programs, `yt-dlp` and
`ffmpeg`, which don't come with Fetchpath.

**Set them up once:** open **Settings → Video and audio** and choose
**Download and set up**. Fetchpath downloads specific versions from their
publishers' official releases and installs them only if they match the
checksums recorded in this version of Fetchpath. It shows each program's
licence before you start. If you already have both programs, choose **I
already have them…** and pick the folder that contains them.

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

The Fetchpath installer connects Fetchpath to Chrome and Microsoft Edge. You
add the extension to your browser once:

1. Open **Settings → Browser extension**. It should say Fetchpath is
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

The first time you send from a website, the browser asks whether Fetchpath may
use that site; this lets it pass along your sign-in for that site only, so
downloads that need you to be signed in keep working. (Video pages are handed
over without your sign-in.)

**Automatic mode.** In the toolbar popup, turn on **Send downloads to Fetchpath
automatically** and allow the permissions the browser asks for. From then on,
downloads you start in the browser go to Fetchpath instead. If Fetchpath can't
take one, the browser downloads it as usual. It's off until you turn it on.

**Why "Developer mode"?** Version 0.1.0 isn't listed in the Chrome Web Store
or Edge Add-ons yet, so the browser treats it as an extension you added
yourself. Chrome may remind you about developer-mode extensions when it
starts; that's expected. Firefox only allows signed add-ons, so Firefox isn't
supported in this version.

## Settings

Open Settings with the **Settings** button or <kbd>Ctrl</kbd>+<kbd>,</kbd>.
Changes save straight away.

| Setting | What it does |
| --- | --- |
| Downloads at the same time | 1 to 8. More at once isn't always faster. |
| Default save folder | Where new downloads and links from your browser go. |
| Retry connection problems automatically | Retries only network and server trouble, up to the number of attempts you choose. Problems that need you, such as a file that already exists or an expired sign-in, always wait for you. |
| Video and audio | Sets up `yt-dlp` and `ffmpeg`. |
| Browser extension | Shows whether the browser connection is installed, and helps you add the extension. |
| Keep running in the notification area | Whether closing the window keeps its icon in the notification area. Downloads continue either way. |
| Ask before removing a finished download | A confirmation before **Remove**. |
| Power mode | Adds a statistics panel and per-download details. Nothing else moves. |
| Appearance | Match Windows, Light or Dark. |

Everything works with the keyboard alone. Press **Keyboard help** for the
shortcuts.

## The command line

The installer also adds a `fetchpath` command. Open a **new** terminal and run:

```powershell
fetchpath download https://example.com/file.zip
```

See [CLI.md](CLI.md) for everything it can do.

## Uninstalling

Use **Settings → Apps → Installed apps** in Windows, find Fetchpath, and
choose **Uninstall**. On the uninstaller's confirmation page, tick **Delete the
application data** only if you also want your download list, history,
settings and media tools removed. Leave it unticked, as it starts, if you're
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
| **"already exists"** | Fetchpath never overwrites. Choose **Choose new path**, or rename or move the old file. |
| **"Edit link" / "Refresh source"** | The link has expired or stopped working. Get a fresh link from the website and paste it. |
| **"Send again from browser"** | The download needed your browser sign-in, and it has expired. Send the link from your browser again. |
| **Connection or server errors** | Choose **Retry**. Turn on automatic retry in Settings to have Fetchpath retry these for you. |
| **Checksum doesn't match** | See [Checking a download against a checksum](#checking-a-download-against-a-checksum). |
| **"Media tools are not set up yet"** | Open **Settings → Video and audio** and choose **Download and set up**. |
| **Send link to Fetchpath is missing** | Check the extension is added and turned on in your browser's extensions page, and that **Settings → Browser extension** shows a check mark. |
| **The toolbar popup says "Fetchpath isn't answering"** | Fetchpath must be installed with its installer on this computer. Reinstall it, then close and reopen the popup. |
| **The `fetchpath` command isn't found** | Open a new terminal after installing. If it still isn't found, your PATH was too long to change safely; add `%LOCALAPPDATA%\Fetchpath` to it yourself. |
| **A download doesn't resume after a restart** | The website may not support resuming, or the file changed. Fetchpath starts again rather than joining two different files. |

Your download list and settings live in `%APPDATA%\app.fetchpath.desktop`.
If the settings file is ever damaged, Fetchpath starts from its defaults and
tells you so in Settings.
