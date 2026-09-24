# Development records

How work is done, and what each task proved. Task state lives only in
[backlog.json](../tasks/backlog.json); these pages hold the commands, results
and limitations behind it.

## How to work

- [Task workflow](WORKFLOW.md) — selecting, delegating, verifying and handing off a task.
- [Building with agents](ORCHESTRATION.md) — model routing and required evidence per kind of work.
- [Task packet template](../tasks/TEMPLATE.md).

## Next phase: engine platform

- [Engine platform design](../architecture/specs/2026-09-24-engine-platform-design.md) — FP-047; tasks FP-048 to FP-069 in the backlog.
- [Engine session](ENGINE-SESSION.md) — FP-048 characterization of the desktop queue, then FP-049.
- [Engine protocol](ENGINE-PROTOCOL.md) — FP-050 protocol v1 and the `EngineClient` interface.
- [Engine host](ENGINE-HOST.md) — FP-053 `fetchpath engine`: single owner, launch or attach, idle exit.

## Release

- [Release candidate](RELEASE-CANDIDATE.md) — FP-018 acceptance matrix and decision.
- [Release readiness](RELEASE-READINESS.md) — FP-035 to FP-039.
- [First end-user test](FIELD-FEEDBACK.md) — FP-040 onward, field findings and fixes.
- [Release polish](RELEASE-POLISH.md) — FP-026 to FP-030.
- [Windows UX and packaging](WINDOWS-PACKAGING.md) — FP-017.

## Engine and features

- [First real download](FIRST-DOWNLOAD.md) and [its review](FIRST-DOWNLOAD-REVIEW.md) — FP-009.
- [Checkpoint and recovery](CHECKPOINT-RECOVERY.md) — FP-011.
- [Desktop queue and recovery](DESKTOP-QUEUE-RECOVERY.md) — FP-012.
- [Browser capture](BROWSER-CAPTURE.md) — FP-013.
- [Media integration](MEDIA-INTEGRATION.md) — FP-014.
- [Adaptive HTTP](ADAPTIVE-HTTP.md) — FP-015.
- [FTP, FTPS and SFTP compatibility](PROTOCOL-COMPATIBILITY.md) — FP-016.
- [Metalink repair](METALINK-REPAIR.md) — FP-019.
- [Content cache and paired LAN](CACHE-AND-LAN.md) — FP-020.
- [Desktop checksum](DESKTOP-CHECKSUM.md) — FP-031.

## Foundation and spikes

- [Foundation validation](FOUNDATION-RESULTS.md) — FP-001 to FP-003.
- [HTTP backend spike](HTTP-BACKEND-SPIKE.md) — FP-004.
- [Job contract review](JOB-CONTRACT-REVIEW.md) — FP-005.
- [Browser handoff spike](BROWSER-HANDOFF-SPIKE.md) — FP-006.
- [Media helper spike](MEDIA-SPIKE.md) — FP-007.

The spike and prototype source was retired on 23 September 2026; it remains
in history at commit `b7a5fc3` (for example
`git show b7a5fc3:tools/spikes/media/run-media-spike.ps1`).

## Where things go

- A new task record: `docs/development/<TOPIC>.md`, listed here.
- Raw evidence a record cites: `docs/development/evidence/<area>/`, never beside source code.
- Implementation plans: [plans](plans/). Design specs: [architecture/specs](../architecture/specs/).
- Scratch output, downloaded toolchains, build trees: ignored `work/`, never cited as the only evidence.
