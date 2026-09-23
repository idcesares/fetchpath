# First end-user test — findings and fixes

Tested by the project owner in Windows Sandbox on 23 September 2026 with the
0.1.0 installer built from `main`. Each finding is reproduced here before it is
fixed, and the fix is proven on the path that failed.

## FP-040 — YouTube downloads failed with "verification_failed"

**Reproduced.** Inspection asked yt-dlp for `--dump-single-json` and kept at
most 512 KiB of its output. For a video with automatic captions the record is
far larger: the TED talk `arj7oStGLkU` produced 848,164 bytes, 162 caption
languages and 44 formats. The JSON was cut off, did not parse, and the failure
surfaced as `verification_failed`. Short or caption-free videos ("Me at the
zoo" 92 KB, Big Buck Bunny 154 KB) worked, which is why fixture and spot tests
passed. The download, merge and ffprobe steps were each run by hand with the
pinned helpers and were not at fault, including a 4K60 HLS variant (1.37 GB).

**Fixed.** Inspection now asks only for the fields the app reads,
`--print "%(.{title,duration,formats})j"` (108 KB for the same talk), and the
per-stream ceiling is 8 MiB as a margin. The ignored network test
`real_captioned_talk_inspects_and_downloads` inspects that talk and downloads
its audio through `MediaJob` with the pinned helpers: passed in 25 s.

**Console windows.** yt-dlp, ffmpeg, ffprobe, `taskkill`, `tar` and `reg.exe`
were started without `CREATE_NO_WINDOW`, so each flashed a console from the
windowed app. All six now carry it; File Explorer keeps its window.

**Still true.** yt-dlp warns that YouTube extraction without a JavaScript
runtime is deprecated and some formats may be missing. It works today without
one; a future yt-dlp may require Deno. Tracked as a risk, not fixed.

## FP-041 — links are analysed automatically

Pasting a link now decides between file and video without the person's help:
a link ending in a file extension stays a file; a page on a known media site
switches to video and is inspected at once; any other page is inspected quietly
when the media tools are set up and becomes a video only if yt-dlp finds one.
The default quality is the best up to 1080p and the file is named after the
video. The type switch remains and a choice made there sticks for that link.

Verified in the running app over the DevTools protocol with the pinned helpers:
pasting the TED talk's YouTube link produced Video, 1080p selected, eight
qualities and a title-based name in 4.6 s with no clicks; a `.exe` link switched
back to a file with its own name; `https://example.com/` stayed a file. Two
defects found on the way and fixed: the file name suggested for a file survived
the switch to video, and content showed beneath the dialog's pinned buttons.

## FP-042 — browser capture from the toolbar (in progress)

The toolbar button did nothing: the extension had no popup and acted only from
a right-click menu nobody was told about. It now has a popup that probes the
Fetchpath host and says whether it is connected, sends the current page, shows
recent sends, and offers an opt-in **Send downloads to Fetchpath
automatically**, which cancels a browser download, hands it to Fetchpath, and
restarts it in the browser if Fetchpath cannot take it. New menus send a page
or a video element (a `blob:` stream falls back to its page). The popup uses
`activeTab`, not `tabs`, to avoid a browsing-history install warning.

On the desktop side, the native host now starts Fetchpath, or brings it forward,
after accepting a capture, breaking away from the browser's job object where
allowed; and a captured page on a media site opens Add download instead of
being saved as its HTML.

Covered by tests: five new extension tests (automatic capture off by default,
hand-off, hand-back on failure, no prompt without site permission, `blob:`
fallback) and two desktop tests (a captured YouTube page goes to review, and
page-versus-file classification). **Not yet run in a real Chromium**; that is
what keeps FP-042 in progress.

## Checks

`cargo test --workspace --locked` 238 passed, 0 failed, 6 ignored; clippy and
fmt clean; `node --test` 29 passed; TypeScript clean; installer rebuilt. The
accessibility harness failed once on the first launch of the new build (its
fixed 700 ms wait after Enter on Add download was too short) and passed on the
rerun; the wait should become a poll.
