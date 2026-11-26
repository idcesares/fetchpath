# Repository map and ownership

Use a single repository while the product shares identity, lifecycle, and release contracts. Add a component only when its first behavior is implemented; no empty crates or placeholder services. `tests/repo/structure.test.mjs` fails when a tracked component is missing from this map, a documentation link is broken, or a workspace entry points at nothing.

## Product

Organized along the Engine 2.0 architectural planes ([engine redesign spec](specs/2026-11-26-engine-redesign.md)):

### Control plane & protocol

| Area | Owns |
|---|---|
| `crates/fetchpath-session` | Single-owner download session: queue model, persistence (JSON v2 today; scalable SQLite WAL/indexed store in `FP-156`), scheduling, priority & bandwidth pacing (`FP-150`), retry classification, rate estimates, history, settings, smart rules (`rules.rs`), media/torrent/multi-source orchestration, browser-inbox ingestion, principals/agent policy/approvals (`policy`, D1/D7), disk reserve (`space`, D6), credentialed provenance (`provenance`, D8), public share links (`share`, `engine/share.rs`, D9), and the protocol engine (command ledger, revisions, durable events, subscriptions). Only `fetchpath engine` hosts it; `apps/desktop` never depends on it |
| `crates/fetchpath-protocol` | Protocol v1 wire types (additive v1.x extensions for D10), command envelopes, replies, durable/ephemeral events, snapshots, errors, length-prefixed framing with size caps, checked-in JSON Schema, `EngineClient` trait, shared display formatting (`describe`, `view`, `qr_image`), and on Windows the authenticated per-user named pipe (`pipe`) and launch-or-attach; no queue logic |
| `crates/fetchpath-browser-inbox` | Browser capture inbox: accepted captures, DPAPI-protected request context, and processed receipts; isolated so the desktop native-messaging host never links `fetchpath-session` |
| `crates/fetchpath-relay` | Share relay (FP-118): enrollment handshake, byte-pipe relay server (no files, no accounts), and engine enrollment client; Linux-portable with no Windows or session dependency (`serve` feature builds `fetchpath-relay`) |

### Data plane & storage

| Area | Owns |
|---|---|
| `crates/fetchpath-core` | Protocol-agnostic data-plane orchestrator: identity types, job lifecycle, unified extent work planner (single-URL + multi-mirror in `FP-153`), RFC 9110 validator & single-pass integrity engine (`FP-146`, `FP-149`), coalesced async write/hash pipeline (`FP-148`), Metalink verification/repair (`verified`), and bounded cookieless photo-page image parsing (`page_image`); no UI or direct vendor transport FFI |
| `crates/fetchpath-storage` | Staging files, positional I/O, Windows NTFS sparse/VDL-aware staging (`FP-148`), generation-fenced checkpoints (v1 prefix today; v2 extent-bitmap in `FP-147`), transactional metadata, and create-only no-overwrite publication |
| `crates/fetchpath-metalink` | Metalink 4 parsing and trusted piece hashes for verified multi-source striping and selective repair |
| `crates/fetchpath-cache` | Bounded content-addressed store keyed by trusted digests, quota, LRU eviction, provenance; no networking |

### Transport plane & adapters

| Area | Owns |
|---|---|
| `crates/fetchpath-http` | Sole owner of HTTP(S) transport (`scheduler`, `controller`, `frontier`, `link`, `single`) and compatibility transports (`compatibility`: FTP/FTPS/SFTP), cancellation, bounded data delivery, shared host connection/DNS/TLS pooling (`FP-151`), and the pluggable `HttpBackend` seam (`libcurl` today; pure-Rust async `hyper` + `h3`/`quinn` in `FP-152`) |
| `adapters/media` | Supervised `yt-dlp`/`ffmpeg` extractor/muxer lifecycle, normalized progress/errors, and pinned helper setup (`setup`, `media-tools.json`) |
| `adapters/torrent` | Isolated `librqbit` helper binary and bounded engine wire contract for torrent and magnet jobs |
| `adapters/lan` | Device identity, pairing, authenticated peer sessions, upload budget; depends on `fetchpath-cache` only, reached by core via `PeerSource` |
| `adapters/providers` | Model and dataset repositories (Hugging Face): link parsing, commit-pinned file listings with sizes/SHA-256 and safe relative paths, plus Xet chunk-dedup evaluation/integration (`FP-155`) |

### Clients, design & skills

| Area | Owns |
|---|---|
| `apps/cli` | `fetchpath` CLI, interactive terminal (`tui`), MCP server (`mcp`), single-owner engine host (`engine`, `src/bin/fetchpath-startup.rs`), loopback web UI listener (`remote`), public share listener (`share`, `sharing.rs`), and guided local agent registration (`agent_setup`) |
| `apps/desktop` | Tauri 2 Windows thin client: links only `fetchpath-protocol` and `fetchpath-browser-inbox` (never session, core, HTTP, or media); commands over the engine protocol, tray/single instance, browser native-messaging host, forked NSIS installer (`src-tauri/installer.nsi`) and helpers (`src-tauri/tools`), native-messaging manifests (`src-tauri/browser/*.json`), UI smoke script (`tools/ui-smoke.ps1`), and generated icons (`src-tauri/icons`) |
| `extensions/browser` | Chrome/Edge capture extension and native control bridge; no payload transport (`tokens.css` synced from `design/tokens.css` via `tools/sync-design-tokens.mjs`) |
| `design` | Design tokens (`tokens.css`), single source of color, type, spacing, radius, elevation, density and motion values for desktop, web, and browser extension ([spec](specs/2026-10-01-design-system-design.md)); no logic |
| `skills` | Portable Agent Skills (`skills/fetchpath/SKILL.md`, `agentskills.io` format) teaching AI agents how to connect to and drive Fetchpath via MCP or CLI |

Split crates only at useful compile/test/ownership boundaries; avoid a crate per abstraction.

## Documentation

| Area | Owns | Primary work |
|---|---|---|
| `PROJECT.md` / `AGENTS.md` / `CONTRIBUTING.md` / `SECURITY.md` | Intent, navigation, operating rules, build commands, vulnerability reporting and scope | Lead |
| `docs/architecture` | Plan, job contract (foundation + D1–D10), repository map, and architectural specs in `docs/architecture/specs` (including the active [Engine 2.0 redesign](specs/2026-11-26-engine-redesign.md)) | Lead |
| `docs/product` | User journeys, UX contract, and platform scope | UX owner, lead review |
| `docs/user` | End-user guide, CLI reference, browser privacy disclosure, and self-hosted relay guide | Release owner |
| `docs/tasks` | `backlog.json` (authoritative task state, dependencies, acceptance, evidence) and handoff template | Lead while agents run |
| `docs/assets` | Static images used by the README (`banner.svg`) | Lead |
| `docs/development` | Workflow, per-area verification records (active Engine 2.0 record: [ENGINE-REDESIGN.md](../development/ENGINE-REDESIGN.md)), raw evidence in `docs/development/evidence`, and [ARCHIVE.md](../development/ARCHIVE.md); indexed by [its README](../development/README.md) | Lead/integrator |
| `docs/plan` | Proposed operational checklists the owner asked to keep in git (e.g. VPS validation); not evidence that a check was run | Lead |

## Tooling and tests

| Area | Owns |
|---|---|
| `tools/tasks.mjs` / `tools/tests.mjs` / `tests/repo` | Backlog integrity and selection, release-asset, relay Linux CI/systemd, and repository-structure checks |
| `tools/bench` / `tests/bench` | Local fixture server, paired download runner (including isolated Chrome native downloads in `tools/bench/chrome-download.mjs`), protocol matrix, NTFS write benchmark, interruption/resume tests, and provider/CDC comparisons |
| `tools/sync-design-tokens.mjs` | Copies design tokens into the browser extension (`--check` reports drift) |
| `tools/licenses` | Third-party notice generation from `Cargo.lock` |
| `tools/install` | Headless PowerShell installer and the agent skill that drives it |
| `tools/media-tools` | Pinning media helpers against their publishers' checksums |
| `tests/installer` | Installer PATH, runtime-import, engine-stop, data-removal, template-fork and component-selection checks |
| `tests/compatibility` | On-machine browser/media/protocol/Windows harnesses with explicit supported/partial/unsupported/untested states |
| `.github` | CI (Windows Rust/desktop/web/task/repo checks, Ubuntu `relay-linux` job) and PR template |
| `.codex` | Project-local Codex tool configuration; no product runtime behavior |

## Local-only areas (ignored)

| Area | Holds |
|---|---|
| `target` | Cargo build output |
| `work` | Generated output, raw logs, downloaded toolchains, temporary probes, and scratch implementation plans (`work/plans`); everything here must be safe to delete |

Retired components and records are indexed in [ARCHIVE.md](../development/ARCHIVE.md).

## Decision record

Accepted: single repository; Rust production direction; three-plane engine separation (Control / Data / Transport); Node built-ins for repository tooling and tests; local JSON task graph. Alternatives rejected: cloud task services, a microservice fleet, a crate per minor abstraction.

Benchmarks exercise production adapters. UI acceptance requires real engine integration, not simulated events.
