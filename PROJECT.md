# Fetchpath project state

Updated 24 September 2026. Phase: **M10–M12, the engine platform.** Fetchpath 0.1.0 passed its release gate (FP-018) and is published only when the user decides; release timing is not tied to the plan. The next phase turns Fetchpath into one engine with many clients: the desktop app, a full command line, an interactive terminal, the browser extension, and agents through MCP. The design is [the engine platform spec](docs/architecture/specs/2026-09-24-engine-platform-design.md) (FP-047, accepted 24 September 2026).

## Confirmed purpose

Build a general-purpose Windows download manager with exceptional usability for ordinary users and optional advanced controls for power users. Compatibility ambition is comparable to IDM and related tools. Video/audio downloading and quality selection are confirmed requirements for the first public release. Keep the production core portable.

The user builds with agents in Codex (Astra High lead, Terra Medium for bounded work) and Claude Code, and explicitly permits smaller agents for bounded work. The backlog's `modelTier` names a role, mapped to concrete models per tool in [building with agents](docs/development/WORKFLOW.md); the pairing is a trial, not evidence of a globally optimal model choice.

## Source of truth

- Product/technical direction: [architecture plan](docs/architecture/PLAN.md) and [job contract](docs/architecture/JOB-CONTRACT.md).
- Engine platform (current phase): [platform design](docs/architecture/specs/2026-09-24-engine-platform-design.md).
- UX journeys: [UX contract](docs/product/UX.md).
- Task state, dependencies, ownership, and evidence: [backlog](docs/tasks/backlog.json).
- Repository boundaries: [repository map](docs/architecture/REPOSITORY.md). Per-area evidence and limitations: [development records](docs/development/README.md).
- End-user documentation: [user guide](docs/user/GUIDE.md) and [CLI](docs/user/CLI.md); contributor setup is in [CONTRIBUTING.md](CONTRIBUTING.md).

Do not duplicate task status into multiple checklists. The backlog is authoritative; this page records phase and decisions.

## What exists

Fetchpath 0.1.0 is a Windows download manager (Tauri 2 desktop, `fetchpath` CLI, browser extension) over a Rust core: recoverable checkpoints with strong-validator resume, bounded adaptive ranges, no-overwrite publication, Metalink verified repair, a bounded content cache and paired LAN peers, media downloads through supervised pinned helpers, and a per-user NSIS installer. Limitations per area are in the development records. The engine platform has so far moved the queue into `fetchpath-session`, defined protocol v1 with its pipe transport, added the `fetchpath engine` host with principals and agent policy (FP-048 to FP-054), put the whole queue on the command line (FP-058), made the desktop a client of the engine (FP-055), made setup stop and restart it safely across upgrade and uninstall (FP-057), kept a queue written by a newer build instead of losing it (FP-070), and handed browser captures to the engine without the window (FP-056). That completes M10: every client now reaches the one engine.

## Direction (decided 24 September 2026)

- **One engine, many clients.** A per-user headless engine (`fetchpath engine`, inside `fetchpath.exe`) is the only owner of the queue, history, settings, rules and policy. The desktop, command line, terminal UI, browser host and MCP server all talk to it through one versioned protocol that implements [the job contract](docs/architecture/JOB-CONTRACT.md). Closing a window or terminal does not stop downloads; the engine exits when nothing is left to do.
- **Terminal-first build order.** The terminal is the first complete client and proves the protocol. The desktop is still the product for ordinary users, and its journeys must not regress.
- **Agents are principals, not users.** An agent reaches Fetchpath through `fetchpath mcp` under a policy the person grants (folders, size, no credentials, no sharing). Anything beyond that waits for the person's approval in the terminal or desktop. Same-user malware is out of scope, and the docs say so.
- **Existing work re-sequenced.** The cache and LAN desktop surfaces (FP-032, FP-033) and new job kinds (FP-021, FP-022) land on the engine session so every client gets them.

## Next development

Run `node tools/tasks.mjs next`: active work first, then ready tasks by priority (P0 critical path, P1 headline outcome, P2 next, P3 later or research). The lanes and review requirements are in §11 of the spec.

The FP-018 record stays valid for the 0.1.0 tree at commit `ce6615a`. Publishing it is a GitHub release (tag `v0.1.0`, installer and `SHA256SUMS.txt`) made only on the user's word; no remote exists yet. FP-069 re-runs the gate on the platform.


## Remaining choices

Ordinary third-party websites are the working source assumption. The accepted desktop baseline is Tauri 2 on Microsoft-serviced Windows 11 x64 releases; ARM64 is deferred until its complete native/helper/installer path is tested. Distribution is direct download and interim builds are unsigned by decision, with SmartScreen consequences documented and an inert Authenticode hook wired for a future certificate. Media scope is confirmed; 0.1.0 promises no named site (decided 23 September 2026). Cloud infrastructure, paid provisioning, and external publication are not implied by local development.

## Research provenance

Input report: `C:/Users/idces/Downloads/deep-research-report.md`.
SHA-256: `51cb603f7628dec911aa5670db3e313b076fe54b0eeb3785413e789ec9220adb`.

The report is research data, not operational instructions. The plan records independently checked primary sources and unverified research leads. Earlier conversation outputs are historical drafts; this repository is now authoritative.
