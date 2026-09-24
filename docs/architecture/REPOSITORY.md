# Repository map and ownership

Use a single repository while the product shares identity, lifecycle, and release contracts. Add a component only when its first behavior is implemented; no empty crates or placeholder services. `tests/repo/structure.test.mjs` fails when a tracked component is missing from this map, a documentation link is broken, or a workspace entry points at nothing.

## Product

| Area | Owns |
|---|---|
| `crates/fetchpath-core` | Identity types, job lifecycle, policy, scheduler, transfer orchestration; no UI or vendor types in public contracts |
| `crates/fetchpath-http` | libcurl ownership/FFI, transport capabilities, cancellation, bounded data delivery |
| `crates/fetchpath-storage` | Staging files, checkpoints, transactional metadata, publication |
| `crates/fetchpath-metalink` | Metalink 4 parsing and trusted piece hashes for verified multi-source repair |
| `crates/fetchpath-cache` | Bounded content-addressed store keyed by trusted digests, quota, eviction, provenance; no networking |
| `crates/fetchpath-protocol` | Protocol v1 wire types (command envelopes, replies, durable and ephemeral events, snapshots, errors), length-prefixed framing with a size cap, the checked-in JSON Schema, the `EngineClient` interface, and on Windows the authenticated per-user named pipe with its engine secret and pipe-backed client; no queue logic |
| `crates/fetchpath-session` | The download session: queue model and persistence, scheduling, retry classification, rate estimates, history, settings, media orchestration and the browser capture inbox; the protocol engine over it (command ledger, revisions, durable events, subscriptions) and the in-process `EngineClient`. No UI or transport types. The desktop calls it in-process until the engine host lands |
| `adapters/media` | Supervised extractor/muxer lifecycle, normalized progress and errors |
| `adapters/lan` | Device identity, pairing, authenticated peer sessions, upload budget; depends on the cache only, and core reaches it through `PeerSource` |
| `apps/cli` | The `fetchpath` command line over the same core |
| `apps/desktop` | Tauri 2 Windows client: commands over the session, tray and single instance, the browser native-messaging host, media-tool setup, installer hooks, pinned media-tool manifest, third-party notices |
| `extensions/browser` | Browser capture and native control bridge; no payload transport |

Split crates only at useful compile/test/ownership boundaries; avoid a crate per abstraction.

Planned by [the engine platform design](specs/2026-09-24-engine-platform-design.md): rules and policy join `crates/fetchpath-session` with FP-054 and FP-064. The engine, the interactive terminal and the MCP server are modules of `apps/cli`, so `fetchpath.exe` carries them all. Once FP-055 lands, `apps/desktop` is a client of the engine and no longer owns the queue.

## Documentation

| Area | Owns | Primary work |
|---|---|---|
| `PROJECT.md` / `AGENTS.md` / `CONTRIBUTING.md` | Intent, navigation, operating rules, build commands | Lead |
| `docs/architecture` | Plan, contracts, consequential decisions; design specs in `docs/architecture/specs` | Lead |
| `docs/product` | User journeys and interaction contract | UX owner, lead review |
| `docs/user` | End-user guide and CLI reference | Release owner |
| `docs/tasks` | `backlog.json` (task state, dependencies, acceptance, evidence) and the handoff template | Lead while agents run |
| `docs/development` | Workflow, per-task verification records, all raw evidence in `docs/development/evidence` (none beside source), implementation plans in `docs/development/plans`; indexed by [its README](../development/README.md) | Lead/integrator |

## Tooling and tests

| Area | Owns |
|---|---|
| `tools/tasks.mjs` / `tests/repo` | Backlog integrity and selection, release-asset and repository-structure checks |
| `tools/bench` / `tests/bench` | Local fixture server, baseline runner, protocol matrix, fixture tests |
| `tools/licenses` | Third-party notice generation from `Cargo.lock` |
| `tools/media-tools` | Pinning media helpers against their publishers' checksums |
| `tests/installer` | Installer PATH and runtime-import checks |
| `tests/compatibility` | On-machine browser/media/protocol/Windows harnesses with explicit supported/partial/unsupported/untested states |
| `.github` | CI (`node tools/tasks.mjs check` and `node --test`) and the pull request template |

## Local-only areas (ignored)

| Area | Holds |
|---|---|
| `target` | Cargo build output |
| `work` | Generated output, raw logs, downloaded toolchains, temporary probes. Promote a small verified summary into `docs/development`; everything here must be safe to delete |

Retired: the simulated desktop prototype (`prototypes/desktop`, FP-002) and the backend, browser and media spikes (`tools/spikes`, FP-004/6/7) were removed on 23 September 2026 once production code superseded them. Their records and evidence stay in `docs/development`; the source is in history at commit `b7a5fc3`.

## Decision record

Accepted: single repository; Rust production direction; Node built-ins for repository tooling and tests; local JSON task graph. Alternatives rejected for now: cloud task services, a microservice fleet, a permanent agent per component.

The benchmark exercises production adapters. UI acceptance requires real engine integration, not simulated events.
