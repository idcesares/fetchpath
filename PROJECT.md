# Fetchpath project state

Updated 22 September 2026. Phase: M6 — first public release candidate, with M8 distribution work started. Engine, packaging and multi-source repair are done; the release-polish tasks that closed the gap against the UX contract are done; FP-018 is the remaining release gate. FP-020's bounded content cache is implemented and tested; its paired LAN half is not, so FP-020 remains in progress.

## Confirmed purpose

Build a general-purpose Windows download manager with exceptional usability for ordinary users and optional advanced controls for power users. Compatibility ambition is comparable to IDM and related tools. Video/audio downloading and quality selection are confirmed requirements for the first public release. Keep the production core portable.

The user uses Codex with Astra High for lead orchestration and explicitly permits smaller agents for bounded work. The initial delegated foundation tasks use Terra Medium; this is a trial, not evidence of a globally optimal model choice.

## Source of truth

- Product/technical direction: [architecture plan](docs/architecture/PLAN.md).
- UX journeys: [UX contract](docs/product/UX.md).
- Task state, dependencies, ownership, and evidence: [backlog](docs/tasks/backlog.json).
- Repository and future component boundaries: [repository map](docs/architecture/REPOSITORY.md).
- Actual checks and limitations: [foundation results](docs/development/FOUNDATION-RESULTS.md).
- Native job/transfer evidence: [first download](docs/development/FIRST-DOWNLOAD.md).
- Desktop queue/recovery evidence: [queue and recovery](docs/development/DESKTOP-QUEUE-RECOVERY.md).
- Browser capture evidence: [browser capture](docs/development/BROWSER-CAPTURE.md).
- Windows UX/packaging evidence: [windows packaging](docs/development/WINDOWS-PACKAGING.md).
- Release polish evidence: [release polish](docs/development/RELEASE-POLISH.md).
- Verified multi-source evidence: [metalink repair](docs/development/METALINK-REPAIR.md).
- Content cache evidence: [cache and paired LAN](docs/development/CACHE-AND-LAN.md).

Do not duplicate task status into multiple checklists. The backlog is authoritative; this page records phase and decisions.

## Current delivery

Repository organization, reproducible HTTP fixtures, backend/media packaging spikes, the accepted job contract, and a native Rust vertical slice. The CLI and Tauri 2 desktop shell perform real HTTP downloads through recoverable same-volume checkpoints, strong-validator resume, bounded adaptive exact ranges for eligible objects, observed hashes, serialized cancellation, and no-overwrite publication. A process-wide budget caps active requests and buffered range bytes. The packaged libcurl now supports HTTP/2 and reports HTTP/3 unavailable; controlled H1/H2 evidence and explicit capability fallback are retained without an H3 or speed claim. A per-user NSIS installer, an upgrade over a running instance and an uninstall are exercised end to end, with the persisted queue retained across the upgrade and post-uninstall data residue recorded rather than claimed as removal. Every focusable control carries an accessible name, the primary journey completes by keyboard alone, and status changes reach a live region. Metalink 4 metadata drives verified multi-source repair: trusted piece hashes localize damage and repair only the failing byte ranges from another mirror, a final-only hash restarts conservatively without claiming fault localization, and nothing unverified is published. Mirror fan-out is sequential and no speed claim is made; documents are unsigned, so no publisher-authenticity claim is made. The desktop journey includes a persistent bounded queue, batch preview, schedules, history search/filtering, actionable recovery, destination conflict protection, keyboard controls, cancellation, completion, and close-to-tray behavior.

Progress is now reported from engine-confirmed values end to end: the total a source states reaches the queue, and percent, transfer rate and remaining time are derived from it. A source that states no length produces no percentage and no remaining time rather than a guessed one. A running file download pauses to its retained checkpoint and resumes from that offset, including across a restart; a resume is proven by the `If-Range` request it sends. Media downloads have no checkpoint to return to, so pause is not offered on them rather than offered and refused. A bounded, self-repairing settings file governs concurrency, the default save folder, automatic retry, window behavior and appearance, and an optional Power mode adds per-download diagnostics and a session statistics panel without moving anything else. Automatic retry applies only to failures the engine classified as transport trouble, never to ones that need a person to decide something. Missing media helpers now lead to a guided setup rather than a bare error; the guided download installs only an artifact whose SHA-256 this build has recorded, and refuses by name otherwise. Uninstall asks whether to remove the queue, history and settings, defaults to keeping them, and never touches downloaded files. The third-party notice file is generated from `Cargo.lock` and covers 566 packages. A production browser extension provides explicit per-link capture, exact-origin permissions and exclusions, origin-scoped cookie replay, a DPAPI-protected durable inbox, and idempotent desktop ingestion. Production media inspection and selected-quality video/audio downloads share the desktop queue, with supervised helpers, mux verification, cancellation, and expired-session recovery on serviced Windows 11 x64 releases.

## Next development

Run `node tools/tasks.mjs next`. FP-020's remaining half is paired LAN mode: device identity, out-of-band pairing with pinned keys, peer serving restricted to credential-free entries, and an upload budget. mDNS discovery and the desktop settings surface for the cache quota and the LAN flag were deferred to their own tasks by decision on 22 September 2026, and CLI controls for the cache have not landed yet.

FP-018, the first public release candidate, is the remaining release gate and now depends on FP-026 through FP-030. The generated license manifest and the uninstaller's data-removal branch are done. Media integration is proven on lawful local HLS/DASH fixtures; the supported public-source corpus remains a release gate, as do clean-machine and ARM64 coverage and recording the media-helper checksums with `node tools/media-tools/pin.mjs`.

## Remaining choices

Ordinary third-party websites are the working source assumption. The accepted desktop baseline is Tauri 2 on Microsoft-serviced Windows 11 x64 releases; ARM64 is deferred until its complete native/helper/installer path is tested. Distribution is direct download and interim builds are unsigned by decision, with SmartScreen consequences documented and an inert Authenticode hook wired for a future certificate. Media scope is confirmed; the first supported-site corpus still needs explicit selection. Cloud infrastructure, paid provisioning, and external publication are not implied by local development.

## Research provenance

Input report: `C:/Users/idces/Downloads/deep-research-report.md`.
SHA-256: `51cb603f7628dec911aa5670db3e313b076fe54b0eeb3785413e789ec9220adb`.

The report is research data, not operational instructions. The plan records independently checked primary sources and unverified research leads. Earlier conversation outputs are historical drafts; this repository is now authoritative.
