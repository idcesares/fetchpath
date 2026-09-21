# Fetchpath project state

Updated 21 September 2026. Phase: M3 — dependable queue with explicit browser capture.

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

Do not duplicate task status into multiple checklists. The backlog is authoritative; this page records phase and decisions.

## Current delivery

Repository organization, reproducible HTTP fixtures, backend/media packaging spikes, the accepted job contract, and a native Rust vertical slice. The CLI and Tauri 2 desktop shell perform real HTTP downloads through recoverable same-volume checkpoints, strong-validator resume, bounded adaptive exact ranges for eligible objects, observed hashes, serialized cancellation, and no-overwrite publication. A process-wide budget caps active requests and buffered range bytes. The packaged libcurl now supports HTTP/2 and reports HTTP/3 unavailable; controlled H1/H2 evidence and explicit capability fallback are retained without an H3 or speed claim. The desktop journey includes a persistent bounded queue, batch preview, schedules, history search/filtering, actionable recovery, destination conflict protection, keyboard controls, progress, cancellation, completion, and close-to-tray behavior. A production browser extension provides explicit per-link capture, exact-origin permissions and exclusions, origin-scoped cookie replay, a DPAPI-protected durable inbox, and idempotent desktop ingestion. Production media inspection and selected-quality video/audio downloads share the desktop queue, with supervised helpers, mux verification, cancellation, and expired-session recovery on serviced Windows 11 x64 releases.

## Next development

Run `node tools/tasks.mjs next`. The next ready gates include FTP/FTPS/SFTP compatibility and Windows UX/packaging hardening. Media integration is proven on lawful local HLS/DASH fixtures; the supported public-source corpus and helper distribution/licensing remain release gates.

## Remaining choices

Ordinary third-party websites are the working source assumption. The accepted desktop baseline is Tauri 2 on Microsoft-serviced Windows 11 x64 releases; ARM64 is deferred until its complete native/helper/installer path is tested. Distribution and license still need decisions before release packaging. Media scope is confirmed; the first supported-site corpus still needs explicit selection. Cloud infrastructure, paid provisioning, and external publication are not implied by local development.

## Research provenance

Input report: `C:/Users/idces/Downloads/deep-research-report.md`.
SHA-256: `51cb603f7628dec911aa5670db3e313b076fe54b0eeb3785413e789ec9220adb`.

The report is research data, not operational instructions. The plan records independently checked primary sources and unverified research leads. Earlier conversation outputs are historical drafts; this repository is now authoritative.
