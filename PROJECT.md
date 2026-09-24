# Fetchpath project state

Updated 24 September 2026. Phase: **M10–M12, the engine platform.** Fetchpath 0.1.0 passed its release gate (FP-018) and is published only when the user decides; release timing is not tied to the plan. The next phase turns Fetchpath into one engine with many clients: the desktop app, a full command line, an interactive terminal, the browser extension, and agents through MCP. The design is [the engine platform spec](docs/architecture/specs/2026-09-24-engine-platform-design.md) (FP-047, accepted 24 September 2026).

## Confirmed purpose

Build a general-purpose Windows download manager with exceptional usability for ordinary users and optional advanced controls for power users. Compatibility ambition is comparable to IDM and related tools. Video/audio downloading and quality selection are confirmed requirements for the first public release. Keep the production core portable.

The user builds with agents in Codex (Astra High lead, Terra Medium for bounded work) and Claude Code, and explicitly permits smaller agents for bounded work. The backlog's `modelTier` names a role, mapped to concrete models per tool in [building with agents](docs/development/ORCHESTRATION.md); the pairing is a trial, not evidence of a globally optimal model choice.

## Source of truth

- Product/technical direction: [architecture plan](docs/architecture/PLAN.md).
- Engine platform (next phase): [platform design](docs/architecture/specs/2026-09-24-engine-platform-design.md).
- UX journeys: [UX contract](docs/product/UX.md).
- Task state, dependencies, ownership, and evidence: [backlog](docs/tasks/backlog.json).
- Repository and future component boundaries: [repository map](docs/architecture/REPOSITORY.md).
- Actual checks and limitations: [foundation results](docs/development/FOUNDATION-RESULTS.md).
- Native job/transfer evidence: [first download](docs/development/FIRST-DOWNLOAD.md).
- Desktop queue/recovery evidence: [queue and recovery](docs/development/DESKTOP-QUEUE-RECOVERY.md).
- Browser capture evidence: [browser capture](docs/development/BROWSER-CAPTURE.md).
- Windows UX/packaging evidence: [windows packaging](docs/development/WINDOWS-PACKAGING.md).
- Release polish evidence: [release polish](docs/development/RELEASE-POLISH.md).
- Release readiness evidence: [release readiness](docs/development/RELEASE-READINESS.md).
- End-user documentation: [user guide](docs/user/GUIDE.md) and [CLI](docs/user/CLI.md); contributor setup is in [CONTRIBUTING.md](CONTRIBUTING.md).
- Verified multi-source evidence: [metalink repair](docs/development/METALINK-REPAIR.md).
- Content cache evidence: [cache and paired LAN](docs/development/CACHE-AND-LAN.md).

Do not duplicate task status into multiple checklists. The backlog is authoritative; this page records phase and decisions.

## Current delivery

Repository organization, reproducible HTTP fixtures, backend/media packaging spikes, the accepted job contract, and a native Rust vertical slice. The CLI and Tauri 2 desktop shell perform real HTTP downloads through recoverable same-volume checkpoints, strong-validator resume, bounded adaptive exact ranges for eligible objects, observed hashes, serialized cancellation, and no-overwrite publication. A process-wide budget caps active requests and buffered range bytes. The packaged libcurl now supports HTTP/2 and reports HTTP/3 unavailable; controlled H1/H2 evidence and explicit capability fallback are retained without an H3 or speed claim. A per-user NSIS installer, an upgrade over a running instance and an uninstall are exercised end to end, with the persisted queue retained across the upgrade and post-uninstall data residue recorded rather than claimed as removal. Every focusable control carries an accessible name, the primary journey completes by keyboard alone, and status changes reach a live region. Metalink 4 metadata drives verified multi-source repair: trusted piece hashes localize damage and repair only the failing byte ranges from another mirror, a final-only hash restarts conservatively without claiming fault localization, and nothing unverified is published. Mirror fan-out is sequential and no speed claim is made; documents are unsigned, so no publisher-authenticity claim is made. The desktop journey includes a persistent bounded queue, batch preview, schedules, history search/filtering, actionable recovery, destination conflict protection, keyboard controls, cancellation, completion, and close-to-tray behavior.

Progress is now reported from engine-confirmed values end to end: the total a source states reaches the queue, and percent, transfer rate and remaining time are derived from it. A source that states no length produces no percentage and no remaining time rather than a guessed one. A running file download pauses to its retained checkpoint and resumes from that offset, including across a restart; a resume is proven by the `If-Range` request it sends. Media downloads have no checkpoint to return to, so pause is not offered on them rather than offered and refused. A bounded, self-repairing settings file governs concurrency, the default save folder, automatic retry, window behavior and appearance, and an optional Power mode adds per-download diagnostics and a session statistics panel without moving anything else. Automatic retry applies only to failures the engine classified as transport trouble, never to ones that need a person to decide something. Missing media helpers now lead to a guided setup rather than a bare error; the guided download installs only an artifact whose SHA-256 this build has recorded, and refuses by name otherwise. Uninstall asks whether to remove the queue, history and settings, defaults to keeping them, and never touches downloaded files. The third-party notice file is generated from `Cargo.lock` and covers 566 packages. A production browser extension provides explicit per-link capture, exact-origin permissions and exclusions, origin-scoped cookie replay, a DPAPI-protected durable inbox, and idempotent desktop ingestion. Production media inspection and selected-quality video/audio downloads share the desktop queue, with supervised helpers, mux verification, cancellation, and expired-session recovery on serviced Windows 11 x64 releases.

## Direction (decided 24 September 2026)

- **One engine, many clients.** A per-user headless engine (`fetchpath engine`, inside `fetchpath.exe`) is the only owner of the queue, history, settings, rules and policy. The desktop, command line, terminal UI, browser host and MCP server all talk to it through one versioned protocol that implements [the job contract](docs/architecture/JOB-CONTRACT.md). Closing a window or terminal does not stop downloads; the engine exits when nothing is left to do.
- **Terminal-first build order.** The terminal is the first complete client and proves the protocol. The desktop is still the product for ordinary users, and its journeys must not regress.
- **Agents are principals, not users.** An agent reaches Fetchpath through `fetchpath mcp` under a policy the person grants (folders, size, no credentials, no sharing). Anything beyond that waits for the person's approval in the terminal or desktop. Same-user malware is out of scope, and the docs say so.
- **Existing work re-sequenced.** The cache and LAN desktop surfaces (FP-032, FP-033) and new job kinds (FP-021, FP-022) land on the engine session so every client gets them.

## Next development

Run `node tools/tasks.mjs next`: it lists active work first, then ready tasks by priority (P0 critical path, P1 headline outcome, P2 next, P3 later or research). With the spec accepted, FP-048 (characterize the desktop queue) and FP-050 (protocol types) can run in parallel. The lanes and review requirements are in §11 of the spec.

The FP-018 record stays valid for the 0.1.0 tree at commit `ce6615a`. Publishing it is a GitHub release (tag `v0.1.0`, installer and `SHA256SUMS.txt`) made only on the user's word; no remote exists yet. FP-069 re-runs the gate on the platform.

The cache and LAN work (FP-020) is done and reviewed; see [cache and paired LAN](docs/development/CACHE-AND-LAN.md). mDNS discovery (FP-034) and research tasks (FP-023 to FP-025) are P3.

## Remaining choices

Ordinary third-party websites are the working source assumption. The accepted desktop baseline is Tauri 2 on Microsoft-serviced Windows 11 x64 releases; ARM64 is deferred until its complete native/helper/installer path is tested. Distribution is direct download and interim builds are unsigned by decision, with SmartScreen consequences documented and an inert Authenticode hook wired for a future certificate. Media scope is confirmed; 0.1.0 promises no named site (decided 23 September 2026). Cloud infrastructure, paid provisioning, and external publication are not implied by local development.

## Research provenance

Input report: `C:/Users/idces/Downloads/deep-research-report.md`.
SHA-256: `51cb603f7628dec911aa5670db3e313b076fe54b0eeb3785413e789ec9220adb`.

The report is research data, not operational instructions. The plan records independently checked primary sources and unverified research leads. Earlier conversation outputs are historical drafts; this repository is now authoritative.
