# Development records

Task state lives only in [backlog.json](../tasks/backlog.json); these pages hold what each area proved and the limitations that still hold. Read only the one your task names.

- [Task workflow](WORKFLOW.md) — how to take, build, verify and close a task; model tiers; keeping the tree small.
- [Task packet template](../tasks/TEMPLATE.md).
- [Retired records](ARCHIVE.md) — removed records and the commit that holds them.

## Active: Engine 2.0 redesign (M20)

- [Engine 2.0 architecture spec](../architecture/specs/2026-11-26-engine-redesign.md) — M20 blueprint (`FP-145`–`FP-156`): scheduler/H2 repair, single-pass hashing, extent-bitmap checkpoints v2, coalesced async I/O, pacing/priorities, shared host pool, pure-Rust H1/H2/H3 backend, unified multi-source/FTP/SFTP/Xet, and scalable persistence.
- [Engine 2.0 verification record](ENGINE-REDESIGN.md) — live paired baselines, M20 gate measurements, and remaining limitations.

## Control plane & platform

- [Engine platform design](../architecture/specs/2026-09-24-engine-platform-design.md) — FP-047; platform foundation.
- [Engine session](ENGINE-SESSION.md) — FP-048/049/051/070/088: queue in `fetchpath-session`, characterization, durable events, wakeups, read-only newer queue guard.
- [Engine protocol](ENGINE-PROTOCOL.md) — FP-050/052: protocol v1, `EngineClient`, authenticated Windows named pipe.
- [Engine host](ENGINE-HOST.md) — FP-053/056/057/058: `fetchpath engine`, launch-or-attach, idle exit, installer lifecycle, CLI.
- [Smart rules](RULES.md) — FP-064/075: site/type/size rules across protocol, desktop, and browser captures.
- [Agents through MCP](MCP.md) — FP-065/066/067: `fetchpath mcp`, principal grants, approvals, boundary review.
- [Instance identity, always on and disk reserve](INSTANCE-AND-LIFECYCLE.md) — FP-101: local instance identity, approval expiry, automatic agent mode, disk reserve.

## Data plane, transport & storage

- [Adaptive HTTP](ADAPTIVE-HTTP.md) — FP-015, FP-084–087: 0.1–0.4 stream-first scheduler, benchmarks, and diagnosed pre-M20 limits.
- [Checkpoint and recovery](CHECKPOINT-RECOVERY.md) — FP-011: staging, v1 prefix checkpoints, create-only publication fence.
- [Metalink repair](METALINK-REPAIR.md) — FP-019: trusted piece hashes and selective repair (unified onto extent planner in `FP-153`).
- [Content cache and paired computers](CACHE-AND-LAN.md) — FP-020, FP-032, FP-033: bounded CAS cache and paired LAN reuse.
- [FTP, FTPS and SFTP compatibility](PROTOCOL-COMPATIBILITY.md) — FP-016: transport layer compatibility (wired into session in `FP-154`).
- [Efficiency research: chunking, deltas, paths, coding](EFFICIENCY-RESEARCH.md) — FP-023, FP-024: CDC/delta measurements and Xet candidate (`FP-155`).

## Clients, adapters & sharing

- [Interactive terminal](TERMINAL.md) — FP-059–063, FP-073: inline panel, dashboard, flows, personalization.
- [Local web UI](WEB-UI.md) — FP-104: `fetchpath.localhost` browser client, sign-in, device principal.
- [Desktop queue and recovery](DESKTOP-QUEUE-RECOVERY.md) — FP-012.
- [Desktop checksum](DESKTOP-CHECKSUM.md) — FP-031.
- [Browser capture](BROWSER-CAPTURE.md) — FP-013, FP-036, FP-056, FP-078, FP-081, FP-142, FP-143.
- [Media integration](MEDIA-INTEGRATION.md) — FP-014, FP-039, FP-081 (`yt-dlp` / `ffmpeg` setup and supervision).
- [Model and dataset providers](PROVIDERS.md) — FP-022: commit-pinned Hugging Face resolution.
- [Torrents and magnets](TORRENTS.md) — FP-021: isolated `librqbit` helper, consent boundaries, publication.
- [Share relay and encryption framing](SHARE-RELAY.md) — FP-116–118, FP-138: chunked AES-GCM framing, Tunnel and Relay modes; unrun VPS checklist is [linux-vps-validation.md](../plan/linux-vps-validation.md).

## Release & quality

- [Local development quality](LOCAL-QUALITY.md) — FP-106–112, FP-136, FP-137, FP-139–141, FP-144: build reproducibility, fixtures, stabilization, cleanup.
- [Release gate](RELEASE-CANDIDATE.md) — 0.4.1 security patch (FP-114), 0.4.0 release gate (FP-113), security scope and publication verdict.
- [Windows UX and packaging](WINDOWS-PACKAGING.md) — FP-017.
- [First end-user test](FIELD-FEEDBACK.md) — FP-040 onward.
