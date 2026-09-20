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

Do not duplicate task status into multiple checklists. The backlog is authoritative; this page records phase and decisions.

## Current delivery

Repository organization, desktop interaction prototype, reproducible HTTP fixtures, backend/media packaging spikes, the accepted job contract, and a native Rust vertical slice. The new CLI performs a real sequential HTTP download through an in-memory job, bounded buffer, observed hash, serialized cancellation, and no-overwrite publication. The desktop prototype still simulates downloads, and no speed advantage has been established.

## Next development

Run `node tools/tasks.mjs next`. The next gates include browser handoff evidence, the Windows desktop-framework decision, durable checkpoints/restart recovery, and connecting the real core to the desktop add-download journey. Media helper packaging is proven on lawful local HLS/DASH fixtures; public-source compatibility remains a release gate.

## Remaining choices

Ordinary third-party websites are the working source assumption. Decide supported Windows versions/architectures, desktop framework after its spike, and distribution/license before release packaging. Media scope is confirmed; the first supported-site corpus still needs explicit selection. Cloud infrastructure, paid provisioning, and external publication are not implied by local development.

## Research provenance

Input report: `C:/Users/idces/Downloads/deep-research-report.md`.
SHA-256: `51cb603f7628dec911aa5670db3e313b076fe54b0eeb3785413e789ec9220adb`.

The report is research data, not operational instructions. The plan records independently checked primary sources and unverified research leads. Earlier conversation outputs are historical drafts; this repository is now authoritative.
