# Fetchpath project state

Updated 24 October 2026. Phase: **0.4.0 published.** Fetchpath 0.4.0 is published on GitHub after owner-authorized validation. It adds in-app Codex and Claude Code connection, a headless installer, and a disk reserve with no limit. The local web UI arrived in 0.3.0. The engine serves the desktop, command line, interactive terminal, browser extension, a local browser page and agents through MCP. The design is [the engine platform spec](docs/architecture/specs/2026-09-24-engine-platform-design.md).

## Confirmed purpose

Build a general-purpose Windows download manager with exceptional usability for ordinary users and optional advanced controls for power users. Compatibility ambition is comparable to IDM and related tools. Video/audio downloading and quality selection are confirmed requirements for the first public release. Keep the production core portable.

The user builds with agents in Codex and Claude Code, using proportional model effort and bounded reviews. Project launch defaults live in `.codex/config.toml`; explicit user selections take precedence. The backlog's `modelTier` names a role, mapped to models and review scope in [building with agents](docs/development/WORKFLOW.md). These choices are not evidence of globally optimal cost or quality.

## Source of truth

- Product/technical direction: [architecture plan](docs/architecture/PLAN.md) and [job contract](docs/architecture/JOB-CONTRACT.md).
- Engine platform: [platform design](docs/architecture/specs/2026-09-24-engine-platform-design.md).
- UX journeys: [UX contract](docs/product/UX.md).
- Task state, dependencies, ownership, and evidence: [backlog](docs/tasks/backlog.json).
- Repository boundaries: [repository map](docs/architecture/REPOSITORY.md). Per-area evidence and limitations: [development records](docs/development/README.md).
- End-user documentation: [user guide](docs/user/GUIDE.md) and [CLI](docs/user/CLI.md); contributor setup is in [CONTRIBUTING.md](CONTRIBUTING.md).

Do not duplicate task status into multiple checklists. The backlog is authoritative; this page records phase and decisions.

## What exists

Fetchpath is a Windows download manager over a portable Rust core. One per-user engine owns the queue, history, settings, rules and agent policy for every client. It provides recoverable, validator-bound HTTP resume, no-overwrite publication, checksum verification, bounded cache reuse, paired LAN sharing, pinned public Hugging Face downloads and media downloads through supervised helpers. The desktop and terminal expose the shared queue and approvals; browser captures reach it even without the window. The [user guide](docs/user/GUIDE.md) covers product behavior, and [development records](docs/development/README.md) hold evidence and limits.

The 0.2.0 release adds torrents and magnets with a packaged isolated helper, shared visual design, Full/Custom installation, optional data cleanup, guided Codex and Claude Code registration, instance identity, optional always-on behavior, approval expiry, explicit automatic mode for agents and disk-space reserve. HTTP connection reuse, scheduling and engine wakeups were improved without a speed claim. Peer discovery defaults on for a person's torrent; agents need explicit policy or approval, browser captures cannot start torrents, and uploading and LAN sharing remain opt-in.

The 0.3.0 release adds the optional local web UI at `fetchpath.localhost` (FP-104): the same list in a browser on this computer, signed in by a single-use link, with view and submit rights only; approvals, settings and sharing stay in the desktop. It also fixes closing to the tray after a second launch and adds tray engine controls and guided Chrome/Edge connection. 0.2.0 cannot read a queue holding a download added from a browser.

The 0.4.0 release adds in-app agent connection, the headless installer, and a disk reserve with no limit. 0.3.0 can still open a 0.4.0 queue.

The [release candidate record](docs/development/RELEASE-CANDIDATE.md) owns the tested revision, artifact and verdict. Guest sign-out/sign-in recovery passed. A physical reboot was not part of that check. Sudden power loss will not be run (FP-077): A04 is an accepted limit, not a demonstrated proof. Claude Code registration and discovery are checked; its current model-driven download flow is explicitly owner-deferred. Remote access was deferred beyond 0.2.0 and is still unbuilt in 0.4.0. `fetchpath hub` keeps the local engine running on this computer and does not listen for other computers. Research recommends no chunking, deltas, multipath or FEC implementation now; a Hugging Face Xet client remains a candidate. Other platforms and mobile are scoped in [PLATFORMS](docs/product/PLATFORMS.md).

## Direction (decided 24 September 2026)

- **One engine, many clients.** A per-user headless engine (`fetchpath engine`, inside `fetchpath.exe`) is the only owner of the queue, history, settings, rules and policy. Every client uses one versioned protocol implementing [the job contract](docs/architecture/JOB-CONTRACT.md). Closing a window or terminal does not stop downloads; the idle engine exits unless always on or sharing keeps it running.
- **Terminal-first build order.** The terminal is the first complete client and proves the protocol. The desktop is still the product for ordinary users, and its journeys must not regress.
- **Agents are principals, not users.** An agent reaches Fetchpath through `fetchpath mcp` under a policy the person grants (folders, size, no credentials, no sharing). Anything beyond that waits for the person's approval in the terminal or desktop. Same-user malware is out of scope, and the docs say so.
- **Existing work re-sequenced.** The cache and LAN desktop surfaces (FP-032, FP-033) and new job kinds (FP-021, FP-022) land on the engine session so every client gets them.

## Next development

`node tools/tasks.mjs next` lists in-progress work, then ready tasks by priority (P0–P3). It is empty: nothing is in progress and no task is ready. FP-091, FP-092, and FP-093 stay blocked. The backlog remains the task list.

0.1.0 was published on 28 September 2026 as [v0.1.0](https://github.com/idcesares/fetchpath/releases/tag/v0.1.0), with its installer and `SHA256SUMS.txt`. On 4 October 2026, [v0.2.0](https://github.com/idcesares/fetchpath/releases/tag/v0.2.0) was published after the recorded release gate and GitHub CI passed. On 9 October 2026, [v0.3.0](https://github.com/idcesares/fetchpath/releases/tag/v0.3.0) was published the same way. On 20 October 2026, [v0.4.0](https://github.com/idcesares/fetchpath/releases/tag/v0.4.0) was published the same way.


## Remaining choices

Ordinary third-party websites are the working source assumption. The desktop baseline is Tauri 2 on Microsoft-serviced Windows 11 x64 releases; ARM64 is deferred until its complete native/helper/installer path is tested. Distribution is direct download and builds are unsigned by decision, with SmartScreen and Smart App Control consequences documented. No named media website, HTTP/3 support or Internet speed advantage is promised. Cloud infrastructure and paid provisioning are not implied by local development.

## Research provenance

Input report: `deep-research-report.md`, kept outside the repository.
SHA-256: `51cb603f7628dec911aa5670db3e313b076fe54b0eeb3785413e789ec9220adb`.

The report is research data, not operational instructions. The plan records independently checked primary sources and unverified research leads. Earlier conversation outputs are historical drafts; this repository is now authoritative.
