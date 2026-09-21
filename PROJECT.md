# Fetchpath project state

Updated 20 September 2026. Phase: M1 — first native download slice.

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

Do not duplicate task status into multiple checklists. The backlog is authoritative; this page records phase and decisions.

## Current delivery

Repository organization, reproducible HTTP fixtures, backend/media packaging spikes, browser handoff evidence, the accepted job contract, and a native Rust vertical slice. The CLI and Tauri 2 desktop shell now perform real sequential HTTP downloads through recoverable same-volume checkpoints, strong-validator HTTP resume, bounded buffers, observed hashes, serialized cancellation, and no-overwrite publication. The desktop journey now includes a persistent bounded queue, batch preview, future schedules with post-sleep catch-up, history search/filtering, actionable recovery, destination conflict protection, keyboard shortcuts, progress, cancellation, completion, and close-to-tray behavior on serviced Windows 11 x64 releases. No speed advantage has been established.

## Next development

Run `node tools/tasks.mjs next`. The next ready gates include browser capture and media download/quality selection. Media helper packaging is proven on lawful local HLS/DASH fixtures; public-source compatibility remains a release gate.

## Remaining choices

Ordinary third-party websites are the working source assumption. The accepted desktop baseline is Tauri 2 on Microsoft-serviced Windows 11 x64 releases; ARM64 is deferred until its complete native/helper/installer path is tested. Distribution and license still need decisions before release packaging. Media scope is confirmed; the first supported-site corpus still needs explicit selection. Cloud infrastructure, paid provisioning, and external publication are not implied by local development.

## Research provenance

Input report: `C:/Users/idces/Downloads/deep-research-report.md`.
SHA-256: `51cb603f7628dec911aa5670db3e313b076fe54b0eeb3785413e789ec9220adb`.

The report is research data, not operational instructions. The plan records independently checked primary sources and unverified research leads. Earlier conversation outputs are historical drafts; this repository is now authoritative.
