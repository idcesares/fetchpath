# Media helper spike (FP-007)

**Result: PASS for a supervised Windows helper proof; not a production integration.**

Run date: 20 September 2026 on Windows 10.0.26200.0, Python 3.14.7, and Node 24.20.0. No `yt-dlp` or `ffmpeg` was installed globally before the spike. All downloaded tools and generated media stay under `work/media-spike/`; evidence is text only under `docs/development/evidence/media/`.

## First-release fixture corpus

This deliberately avoids third-party media downloads. FFmpeg generates a 12-second 640×360 moving test pattern and a synthetic 880 Hz tone, then derives the following localhost-only corpus:

| Case | Source | What it proves | Result |
| --- | --- | --- | --- |
| Direct MP4 | `direct.mp4` | Generic direct-media path and cancellation target | PASS: cancellation left a 130,048-byte `.part`, never a completed output |
| Muxed HLS VOD | `hls/master.m3u8` | HLS handling | PASS: `hls.mp4`, 12.027937 s |
| DASH | `dash/manifest.mpd` | Separate video/audio formats, enumeration, and FFmpeg merge | PASS: two formats (360p H.264 video; M4A AAC audio) and `dash-muxed.mp4`, 12 s |
| DASH audio-only | same MPD | Audio selection and FFmpeg extraction | PASS: `dash-audio.mp3`, 12 s |
| Missing local playlist | `/missing.m3u8` | Bounded helper failure reporting | PASS: helper exit 1, captured as an error event |

This is the initial lawful fixture corpus. It validates the helper boundary and media assembly, not a public-site support claim. Before launch, add separately authorized, stable public-source cases and record their terms, location/availability risks, and expected results.

## Reproduce

From the repository root, acquire the helpers into the isolated work directory. The FFmpeg package URL and hash are from the current `winget show --id Gyan.FFmpeg --exact` metadata; the yt-dlp release checksum is downloaded with the Windows executable.

```powershell
New-Item -ItemType Directory -Force work/media-spike/tools | Out-Null
Invoke-WebRequest https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp.exe -OutFile work/media-spike/tools/yt-dlp.exe
Invoke-WebRequest https://github.com/yt-dlp/yt-dlp/releases/latest/download/SHA2-256SUMS -OutFile work/media-spike/tools/yt-dlp-SHA2-256SUMS.txt
Invoke-WebRequest https://github.com/GyanD/codexffmpeg/releases/download/9.0.1/ffmpeg-9.0.1-full_build.zip -OutFile work/media-spike/tools/ffmpeg-9.0.1-full_build.zip
powershell -ExecutionPolicy Bypass -File tools/spikes/media/run-media-spike.ps1
```

`tools/spikes/media/run-media-spike.ps1` extracts FFmpeg locally, generates all fixtures under `work/media-spike/fixtures`, runs its sibling `supervise-helper.mjs`, and writes version, hash, duration, environment, and JSONL event evidence. The runner starts a loopback-only HTTP server and passes `--ignore-config`, a scoped `--ffmpeg-location`, newline progress templates, and no user URLs or credentials. The script explicitly sets FFmpeg's DASH working directory to `fixtures/dash`, so generated segments never reach the repository root.

## Tested versions and integrity record

| Component | Version / SHA-256 | Source / observation |
| --- | --- | --- |
| yt-dlp Windows x64 executable | `2026.08.19`; `66674953fe251b89f4d08c5f0e35e0728679bd67ab3d7d05c0562af101dd3e7a` | [yt-dlp release files and checksums](https://github.com/yt-dlp/yt-dlp#release-files); local SHA-256 matched the downloaded `SHA2-256SUMS` line |
| FFmpeg portable full build | `9.0.1`; archive `2e8e28af97c2ae338ccef92e36da9b2a4cd21d0cad9dde093545606cb07f5b00` | [Gyan FFmpeg 9.0.1 release archive](https://github.com/GyanD/codexffmpeg/releases/download/9.0.1/ffmpeg-9.0.1-full_build.zip); same hash published by winget metadata |
| Fixture input | `direct.mp4`; `94e38cacc6cbfefe54d8c37e6d7337be194f32fae958d2867a03b70e2c2a1bcf` | Generated locally by the spike |

The locally calculated hashes make reruns comparable; they are not publisher-authenticity proof. yt-dlp publishes signed checksum manifests and its public key, but GPG was not present in this environment, so signature verification is **NOT RUN**. A packaging pipeline must verify the published signature before accepting an update.

## Helper contract demonstrated

The Node-only spike supervisor records JSONL `start`, parsed `progress`, raw helper output, `cancel-requested`, and `exit` events. It does not parse stdout as an authority to mark a user job complete; only a zero exit plus post-run output inspection is treated as a successful fixture result.

- **Quality enumeration:** `yt-dlp -J` returned two DASH representations (audio-only M4A and 640×360 MP4 video). A production adapter should expose only the representations from this inspected payload and retain the helper version and selection expression.
- **Mux:** `-f bestvideo+bestaudio --merge-output-format mp4` produced a 12-second MP4 through FFmpeg.
- **Audio:** `-f bestaudio -x --audio-format mp3` produced a 12-second MP3 through FFmpeg.
- **Cancellation:** after the first structured progress event, the supervisor calls Windows `taskkill /pid <helper> /t /f`. The helper exited 1 and left a `.part`; a real coordinator must keep this partial output unpublishable and choose explicit resume/delete policy.
- **Failure:** a loopback 404 exited 1. Production needs a typed error translation layer, redacted diagnostics, retry policy, and source-specific recovery guidance.

## Packaging, update, and license requirements

yt-dlp documents that the Windows x64 standalone executable is its recommended Windows build, that FFmpeg/ffprobe are needed for merging separate streams and post-processing, and that site support changes. It also says its release executable can update through its channel mechanism; do **not** allow that process to self-update inside Fetchpath. Fetchpath should instead ship pinned helper/FFmpeg versions, check vendor metadata and signatures in a controlled updater, stage and smoke-test replacements, retain rollback, and record the exact helper version per job. [yt-dlp installation, update, dependencies, and licensing](https://github.com/yt-dlp/yt-dlp#installation).

The yt-dlp source is Unlicense, but its PyInstaller release executable includes GPLv3+ code according to yt-dlp. The Gyan full FFmpeg build is listed by winget as GPL-3.0. Any bundled distribution therefore requires a complete license and source/offering review before shipping; this spike makes no legal conclusion. The first release must also define Windows architectures, supported OS baseline, update ownership, CVE response timing, component notices, and any site-specific terms review.

## Limits and next gate

PASS is limited to local generated direct/HLS/DASH media with one x64 Windows helper build. It does not validate extraction from third-party sites, browser cookies/authentication, DRM-protected content, live streams, geo/age restrictions, manifests with expiring keys, multi-job budgets, resumability after cancellation, crash recovery, installer behavior, code signing, or update signature verification. No production backend or UI integration was added.

Before depending on this in M5, define the public supported-source corpus; add approved real-source compatibility cases; decide distribution licensing; and implement a narrow supervisor/IPC contract that owns process-tree termination, bounded concurrency, redaction, output publication, and controlled updates.
