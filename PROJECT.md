# Fetchpath project state

Updated 4 October 2026. Phase: **0.2.0 release preparation.** Fetchpath 0.1.0 is published on GitHub; 0.2.0 is undergoing owner-authorized validation and documentation before a release verdict. Its engine serves the desktop, command line, interactive terminal, browser extension and agents through MCP. The design is [the engine platform spec](docs/architecture/specs/2026-09-24-engine-platform-design.md).

## Confirmed purpose

Build a general-purpose Windows download manager with exceptional usability for ordinary users and optional advanced controls for power users. Compatibility ambition is comparable to IDM and related tools. Video/audio downloading and quality selection are confirmed requirements for the first public release. Keep the production core portable.

The user builds with agents in Codex and Claude Code, using proportional model effort and bounded reviews. Project launch defaults live in `.codex/config.toml`; explicit user selections take precedence. The backlog's `modelTier` names a role, mapped to models and review scope in [building with agents](docs/development/WORKFLOW.md). These choices are not evidence of globally optimal cost or quality.

## Source of truth

- Product/technical direction: [architecture plan](docs/architecture/PLAN.md) and [job contract](docs/architecture/JOB-CONTRACT.md).
- Engine platform (current phase): [platform design](docs/architecture/specs/2026-09-24-engine-platform-design.md).
- UX journeys: [UX contract](docs/product/UX.md).
- Task state, dependencies, ownership, and evidence: [backlog](docs/tasks/backlog.json).
- Repository boundaries: [repository map](docs/architecture/REPOSITORY.md). Per-area evidence and limitations: [development records](docs/development/README.md).
- End-user documentation: [user guide](docs/user/GUIDE.md) and [CLI](docs/user/CLI.md); contributor setup is in [CONTRIBUTING.md](CONTRIBUTING.md).

Do not duplicate task status into multiple checklists. The backlog is authoritative; this page records phase and decisions.

## What exists

Fetchpath is a Windows download manager over a portable Rust core. One per-user engine owns the queue, history, settings, rules and agent policy for every client. It provides recoverable, validator-bound HTTP resume, no-overwrite publication, checksum verification, bounded cache reuse, paired LAN sharing, pinned public Hugging Face downloads and media downloads through supervised helpers. The desktop and terminal expose the shared queue and approvals; browser captures reach it even without the window. The [user guide](docs/user/GUIDE.md) covers product behavior, and [development records](docs/development/README.md) hold evidence and limits.

The 0.2.0 candidate adds torrents and magnets with a packaged isolated helper, shared visual design, Full/Custom installation, optional data cleanup, guided Codex and Claude Code registration, instance identity, optional always-on behavior, approval expiry, explicit automatic mode for agents and disk-space reserve. HTTP connection reuse, scheduling and engine wakeups were improved without a speed claim. Peer discovery defaults on for a person's torrent; agents need explicit policy or approval, browser captures cannot start torrents, and uploading and LAN sharing remain opt-in.

The [release candidate record](docs/development/RELEASE-CANDIDATE.md) owns the tested revision, artifact and verdict. Real reboot/sign-in recovery and sudden power-loss evidence remain incomplete. Claude Code registration and discovery are checked; its current model-driven download flow is explicitly owner-deferred. Remote hub work is deferred beyond 0.2.0. Research recommends no chunking, deltas, multipath or FEC implementation now; a Hugging Face Xet client remains a candidate. Other platforms and mobile are scoped in [PLATFORMS](docs/product/PLATFORMS.md).

## Direction (decided 24 September 2026)

- **One engine, many clients.** A per-user headless engine (`fetchpath engine`, inside `fetchpath.exe`) is the only owner of the queue, history, settings, rules and policy. Every client uses one versioned protocol implementing [the job contract](docs/architecture/JOB-CONTRACT.md). Closing a window or terminal does not stop downloads; the idle engine exits unless always on or sharing keeps it running.
- **Terminal-first build order.** The terminal is the first complete client and proves the protocol. The desktop is still the product for ordinary users, and its journeys must not regress.
- **Agents are principals, not users.** An agent reaches Fetchpath through `fetchpath mcp` under a policy the person grants (folders, size, no credentials, no sharing). Anything beyond that waits for the person's approval in the terminal or desktop. Same-user malware is out of scope, and the docs say so.
- **Existing work re-sequenced.** The cache and LAN desktop surfaces (FP-032, FP-033) and new job kinds (FP-021, FP-022) land on the engine session so every client gets them.

## Next development

Run `node tools/tasks.mjs next`: active work first, then ready tasks by priority (P0 critical path, P1 headline outcome, P2 next, P3 later or research). The lanes and review requirements are in §11 of the spec.

0.1.0 was published on 28 September 2026 as [v0.1.0](https://github.com/idcesares/fetchpath/releases/tag/v0.1.0), with its installer and `SHA256SUMS.txt`. On 4 October the owner authorized 0.2.0 closure and publication only if its recorded release verdict permits it.


## Remaining choices

Ordinary third-party websites are the working source assumption. The desktop baseline is Tauri 2 on Microsoft-serviced Windows 11 x64 releases; ARM64 is deferred until its complete native/helper/installer path is tested. Distribution is direct download and builds are unsigned by decision, with SmartScreen and Smart App Control consequences documented. No named media website, HTTP/3 support or Internet speed advantage is promised. Cloud infrastructure and paid provisioning are not implied by local development.

## Research provenance

Input report: `deep-research-report.md`, kept outside the repository.
SHA-256: `51cb603f7628dec911aa5670db3e313b076fe54b0eeb3785413e789ec9220adb`.

The report is research data, not operational instructions. The plan records independently checked primary sources and unverified research leads. Earlier conversation outputs are historical drafts; this repository is now authoritative.
