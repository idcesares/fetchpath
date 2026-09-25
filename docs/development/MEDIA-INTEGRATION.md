# Media download integration

Verified on 2026-09-21 for FP-014.

## User flow

The desktop composer has a **Video or audio** mode. The user enters one HTTP(S) source and chooses **Inspect link**. Fetchpath displays only variants returned by the media engine, including an audio-only choice when the source exposes audio. A queued media item retains the selected variant identity and label. Immediately before downloading, the adapter inspects the source again and rejects a missing or changed selection rather than silently substituting another quality.

Media and ordinary file downloads share the bounded desktop queue, history, schedule, cancellation, retry, no-overwrite, and restart-recovery paths. An expired session becomes an actionable `refresh_source` failure. Private query values remain redacted and are not persisted.

## Helper boundary

`fetchpath-media` supervises `yt-dlp`, FFmpeg, and ffprobe as untrusted child processes:

- commands receive argument arrays without a shell and `--ignore-config` prevents ambient configuration from changing behavior;
- stdout and stderr are drained with a 512 KiB retention bound;
- inspection has a 60-second limit and downloads have a six-hour upper bound;
- Windows helpers run in a new process group and cancellation terminates the process tree;
- work stays in a unique hidden directory beside the destination;
- ffprobe must confirm positive duration and the expected audio/video streams;
- publication uses a same-volume, create-only hard link, so an existing or racing destination is never replaced;
- the work directory is removed after success, cancellation, or failure;
- helper output and source URLs are not placed in user-facing errors or queue history.

The application discovers tools from `FETCHPATH_YT_DLP` plus `FETCHPATH_FFMPEG_DIR`, from a directory named by `FETCHPATH_MEDIA_TOOLS_DIR`, or from a packaged `media-tools` directory beside the application executable. A tool directory contains `yt-dlp.exe` and either FFmpeg executables directly or in `bin/`. Fetchpath does not invoke helper self-update.

## Verification

The production adapter was exercised against the repository's locally generated, non-copyright DASH fixture. Engine inspection returned a 360p video choice and best-available audio choice. The selected video path downloaded separate DASH streams, muxed them to MP4, verified H.264 video plus AAC audio, and published 1,974,606 bytes. The audio path extracted MP3, verified an audio stream, and published 100,812 bytes. Both durations were 12 seconds. Exact hashes and tool identities are in `evidence/media/media-integration.json`.

Automated tests cover variant sorting/deduplication, expected-stream verification, input protocol boundaries, output redaction, helper crash classification, expired-session classification, Windows process-tree cancellation with no publication or retained staging, desktop queue recovery action, and the pre-existing queue/browser contract.

## Remaining release gates

This proves the production integration against lawful local DASH plus the earlier HLS/DASH spike coverage. It does not claim compatibility with arbitrary public sites. Before a public release, the project still needs an explicit supported-source corpus, repeatable compatibility runs for that corpus, and a distribution/licensing decision for the helper binaries. Helper updates must remain controlled and compatibility-tested.

## Pinned helpers (FP-039, 23 September 2026)

yt-dlp 2026.08.19 and the GyanD ffmpeg 9.0.2 essentials build are pinned in `apps/desktop/src-tauri/media-tools.json` against their publishers' checksum files. The first real guided install exposed and fixed an archive-flattening defect; the ignored network test `guided_install_fetches_verifies_and_runs_the_pinned_helpers` now passes. No public site is promised. See [release readiness](ARCHIVE.md).
