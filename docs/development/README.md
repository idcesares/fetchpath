# Development records

Task state lives only in [backlog.json](../tasks/backlog.json); these pages
hold what each area proved and the limitations that still hold. Read only the
one your task names.

- [Task workflow](WORKFLOW.md) — how to take, build, verify and close a task; model tiers; keeping the tree small.
- [Task packet template](../tasks/TEMPLATE.md).
- [Retired records](ARCHIVE.md) — removed records and the commit that holds them.

## Engine platform (current phase)

- [Engine platform design](../architecture/specs/2026-09-24-engine-platform-design.md) — FP-047; its task map (§11) lists every platform task.
- [Engine session](ENGINE-SESSION.md) — FP-048/049/051/070: the queue in `fetchpath-session`, its characterization, a newer build's queue kept, and open findings.
- [Engine protocol](ENGINE-PROTOCOL.md) — FP-050/052: protocol v1, `EngineClient`, the pipe.
- [Engine host](ENGINE-HOST.md) — FP-053: `fetchpath engine`, launch or attach, idle exit; FP-056/057: browser host and installer lifecycle; FP-058: the command line through the engine.
- [Interactive terminal](TERMINAL.md) — FP-059 to FP-063 and FP-073: inline panel, prompt and `/` commands, dashboard, flows, personalization, remembered context, plain mode, and the review of its additions.
- [Smart rules](RULES.md) — FP-064: rules by site, type or size, applied by the engine to protocol jobs; FP-075 extends them to the desktop and browser captures.
- [Agents through MCP](MCP.md) — FP-065/066/067: `fetchpath mcp`, what an agent is shown, the person's access and approval surfaces, and the boundary review with its residual risks.

## Features

- [Checkpoint and recovery](CHECKPOINT-RECOVERY.md) — FP-011.
- [Desktop queue and recovery](DESKTOP-QUEUE-RECOVERY.md) — FP-012.
- [Browser capture](BROWSER-CAPTURE.md) — FP-013, FP-036.
- [Media integration](MEDIA-INTEGRATION.md) — FP-014, FP-039.
- [Adaptive HTTP](ADAPTIVE-HTTP.md) — FP-015.
- [FTP, FTPS and SFTP compatibility](PROTOCOL-COMPATIBILITY.md) — FP-016.
- [Metalink repair](METALINK-REPAIR.md) — FP-019.
- [Content cache and paired computers](CACHE-AND-LAN.md) — FP-020, FP-032, FP-033.
- [Desktop checksum](DESKTOP-CHECKSUM.md) — FP-031.
- [Model and dataset providers](PROVIDERS.md) — FP-022.

## Release

- [Release gate](RELEASE-CANDIDATE.md) — the 0.1.0 acceptance matrix on the engine platform and its verdict (FP-069).
- [Windows UX and packaging](WINDOWS-PACKAGING.md) — FP-017.
- [First end-user test](FIELD-FEEDBACK.md) — FP-040 onward.
