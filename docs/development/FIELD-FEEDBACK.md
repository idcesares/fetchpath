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
