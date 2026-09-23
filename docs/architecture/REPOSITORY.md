# Repository map and ownership

Use a single repository while the product shares identity, lifecycle, and release contracts. Add components only when their first behavior is implemented. No empty production crates or placeholder services are needed.

| Existing area | Owns | Primary work |
|---|---|---|
| PROJECT.md / AGENTS.md | Intent, navigation, operating rules | Lead |
| docs/architecture | Architecture and consequential decisions | Lead |
| docs/product | User journeys and interaction contract | UX owner, lead review |
| docs/tasks/backlog.json | Task state, dependency graph, acceptance, evidence | Lead while agents run |
| docs/development | Workflow and verification records | Lead/integrator |
| prototypes/desktop | Disposable simulated interaction prototype | UX owner |
| tools/bench / tests/bench | Local server, baseline runner, fixture tests | Benchmark owner |
| tools/tasks.mjs / tests/repo | Backlog integrity and selection | Tooling owner |
| work (ignored) | Generated output, local artifacts, temporary probes | Task-scoped |

## Production areas to create as needed

- `crates/fetchpath-core`: identity types, job lifecycle, policy, scheduler; no UI or vendor-specific types in public contracts.
- `crates/fetchpath-http`: libcurl ownership/FFI, transport capabilities, cancellation and bounded data delivery.
- `crates/fetchpath-storage`: staging files, checkpoints, transactional metadata and publication.
- `apps/desktop`: production Windows interface/host after framework selection; consumes shared commands/events.
- `apps/cli`: engineering and power-user interface over the same core.
- `extensions/browser`: extension capture and native control bridge, no payload transport.
- `adapters/media`: supervised extractor/muxer lifecycle and normalized progress/errors.
- `crates/fetchpath-cache`: bounded content-addressed store keyed by trusted digests, quota, eviction and immutable provenance; no networking.
- `adapters/lan`: device identity, pairing, authenticated peer sessions and upload budget; depends on the cache only, and core reaches it through the `PeerSource` trait.
- `tests/compatibility`: browser/auth/media/protocol matrices; explicit supported/partial/unsupported/untested states.

Split crates only at useful compile/test/ownership boundaries. One small core crate may initially contain several modules; avoid a crate per abstraction. A Cargo workspace and lockfile are introduced with the first real Rust slice, not merely to make the tree look complete.

## Decision record

Accepted for foundation: single repository; Rust production direction; Node built-ins for disposable harness/tooling; local JSON task graph; simulated prototype kept separate from production UI. Alternatives rejected for now: cloud task services, a microservice fleet, a permanent agent per component, and choosing a desktop framework without testing Windows UX/packaging.

The benchmark must exercise production adapters when they exist. Until then its results describe the fixture and external clients only. UI prototypes may use simulated events, explicitly labeled; production acceptance requires real engine integration.
