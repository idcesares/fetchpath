# Development records

Task state lives only in [backlog.json](../tasks/backlog.json); these pages
hold what each area proved and the limitations that still hold. Read only the
one your task names.

- [Task workflow](WORKFLOW.md) — how to take, build, verify and close a task; model tiers; keeping the tree small.
- [Task packet template](../tasks/TEMPLATE.md).
- [Retired records](ARCHIVE.md) — removed records and the commit that holds them.

## Engine platform (current phase)

- [Engine platform design](../architecture/specs/2026-09-24-engine-platform-design.md) — FP-047; tasks FP-048 to FP-070.
- [Engine session](ENGINE-SESSION.md) — FP-048/049/051: the queue in `fetchpath-session`, its characterization and open findings.
- [Engine protocol](ENGINE-PROTOCOL.md) — FP-050/052: protocol v1, `EngineClient`, the pipe.
- [Engine host](ENGINE-HOST.md) — FP-053: `fetchpath engine`, launch or attach, idle exit; FP-058: the command line through the engine.
- [Interactive terminal](TERMINAL.md) — FP-059: inline panel, prompt and `/` commands, plain mode.

## Features

- [Checkpoint and recovery](CHECKPOINT-RECOVERY.md) — FP-011.
- [Desktop queue and recovery](DESKTOP-QUEUE-RECOVERY.md) — FP-012.
- [Browser capture](BROWSER-CAPTURE.md) — FP-013, FP-036.
- [Media integration](MEDIA-INTEGRATION.md) — FP-014, FP-039.
- [Adaptive HTTP](ADAPTIVE-HTTP.md) — FP-015.
- [FTP, FTPS and SFTP compatibility](PROTOCOL-COMPATIBILITY.md) — FP-016.
- [Metalink repair](METALINK-REPAIR.md) — FP-019.
- [Content cache and paired LAN](CACHE-AND-LAN.md) — FP-020.
- [Desktop checksum](DESKTOP-CHECKSUM.md) — FP-031.

## Release

- [Release candidate](RELEASE-CANDIDATE.md) — FP-018 acceptance matrix and decision; FP-069 re-runs it.
- [Windows UX and packaging](WINDOWS-PACKAGING.md) — FP-017.
- [First end-user test](FIELD-FEEDBACK.md) — FP-040 onward.
