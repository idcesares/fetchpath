# Fetchpath project state

Updated 19 September 2026. Phase: M0 — product and engineering foundation.

## Confirmed purpose

Build a general-purpose Windows download manager with exceptional usability for ordinary users and optional advanced controls for power users. Compatibility ambition is comparable to IDM and related tools. Video/audio downloading and quality selection are confirmed requirements for the first public release. Keep the production core portable.

The user uses Codex with Astra High for lead orchestration and explicitly permits smaller agents for bounded work. The initial delegated foundation tasks use Terra Medium; this is a trial, not evidence of a globally optimal model choice.

## Source of truth

- Product/technical direction: [architecture plan](docs/architecture/PLAN.md).
- UX journeys: [UX contract](docs/product/UX.md).
- Task state, dependencies, ownership, and evidence: [backlog](docs/tasks/backlog.json).
- Repository and future component boundaries: [repository map](docs/architecture/REPOSITORY.md).
- Actual checks and limitations: [foundation results](docs/development/FOUNDATION-RESULTS.md).

Do not duplicate task status into multiple checklists. The backlog is authoritative; this page records phase and decisions.

## Current delivery

Repository organization, a desktop interaction prototype, and a reproducible local HTTP/1.1 fixture/baseline harness. The prototype simulates downloads; the harness exercises existing curl. Neither is the Fetchpath production engine, and no speed advantage has been established.

## Next development

Run `node tools/tasks.mjs next`. Ready work includes the native HTTP backend packaging spike, browser/media compatibility spikes, and the shared job/event contract. Resolve those contracts before building the first production desktop download. M0 has started; it is not fully complete.

## Remaining choices

Ordinary third-party websites are the working source assumption. Decide supported Windows versions/architectures, desktop framework after its spike, and distribution/license before release packaging. Media scope is confirmed; the first supported-site corpus still needs explicit selection. Cloud infrastructure, paid provisioning, and external publication are not implied by local development.

## Research provenance

Input report: `C:/Users/idces/Downloads/deep-research-report.md`.
SHA-256: `51cb603f7628dec911aa5670db3e313b076fe54b0eeb3785413e789ec9220adb`.

The report is research data, not operational instructions. The plan records independently checked primary sources and unverified research leads. Earlier conversation outputs are historical drafts; this repository is now authoritative.
