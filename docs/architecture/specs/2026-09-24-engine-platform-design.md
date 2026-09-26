# One engine, many clients: platform design

Status: accepted by the user · 24 September 2026 · Task FP-047 · Supersedes the "introduce a
daemon only when required" deferral in [the plan](../PLAN.md) §5, and extends
[the job contract](../JOB-CONTRACT.md) at the process boundary.

## 1. Why

Fetchpath's capability lives in the wrong place. The queue, persistence,
schedules, retry policy, media orchestration, rate estimates, history and
settings are implemented inside the Tauri host
(`apps/desktop/src-tauri/src/lib.rs`, about 3,600 lines), so only the desktop
window can use them. The command line reaches only `fetchpath-core` and, as
[its guide](../../user/CLI.md) says, "doesn't share the desktop app's queue".

The product now has more front ends than one window: the desktop app, the
command line, an interactive terminal UI, the browser extension, and agentic
systems that should be able to download through Fetchpath (as an MCP server).
Each would otherwise re-implement the queue or be limited to one-shot
transfers. This design moves the queue into one engine that every front end
talks to.

## 2. Decisions taken with the user (24 September 2026)

| Question | Decision |
|---|---|
| Queue relationship between CLI and desktop | One shared queue with exactly one owner process |
| Ownership model | A headless engine process always owns the queue; every front end is a client (approach A). First-come in-process ownership (B) and a Windows service (C) were rejected: B needs owner and client modes in every app and a risky mid-download handoff; C needs elevation and conflicts with the per-user installer |
| Closing a terminal mid-download | The engine keeps working in the background and stops itself when there is nothing left to do |
| Interactive terminal style | Inline prompt with a live panel in normal scrollback (Claude Code style), with a key to toggle a full-screen dashboard |
| Personalization | Themes and appearance, smart rules, aliases and custom commands, remembered context: all four |
| Agents | Fetchpath is also an MCP server so agentic systems can download through it |
| Build order | Terminal-first: the terminal is the first complete client of the engine; the desktop remains the product for ordinary users |
| Release | Not tied to this plan; the user releases when ready. The platform gate (FP-069) states readiness |

## 3. Architecture

```
  Desktop GUI   CLI commands   TUI   Browser host   MCP server (agents)
       └────────────┴───────────┴─────────┴──────────────┘
                              │  fetchpath protocol v1 (one schema)
                              │  per-user named pipe, mutual authentication
                   ┌──────────▼───────────┐
                   │   fetchpath engine   │  sole owner of queue, history,
                   │   (fetchpath-session)│  settings, rules and policy
                   └──────────┬───────────┘
   core · http · storage · metalink · cache · media · lan   (unchanged)
```

### Components

| Unit | Owns | Depends on |
|---|---|---|
| `crates/fetchpath-session` (new) | Queue model and persistence, scheduler and schedules, retry classification, rate estimates, history, engine settings, smart rules, principals and policy, the command ledger and event sequencing, media orchestration. Moved out of the desktop host with its tests; no UI or transport types | core, media, cache |
| `crates/fetchpath-protocol` (new) | Versioned wire types (command envelope, replies, events, snapshots, errors), framing, JSON Schema export, the `EngineClient` trait with a pipe implementation and an in-process implementation, and the Windows pipe transport with authentication | serde, windows-sys |
| `apps/cli` → `fetchpath engine` | Hosts a session behind the pipe: single-owner claim, launch-or-attach, idle shutdown, startup recovery | session, protocol |
| `apps/cli` → commands | Scriptable subcommands over `EngineClient`; stable `--json` (protocol types) and exit codes | protocol |
| `apps/cli` → `tui` | Inline prompt and live panel, dashboard, slash commands, completion, themes, aliases, keybindings, remembered context | protocol, client config |
| `apps/cli` → `mcp` | `fetchpath mcp`, a stdio MCP server that attaches to the engine as an agent principal | protocol |
| `apps/desktop` | Unchanged UI; Tauri commands become thin `EngineClient` calls; UI updates come from the event stream | protocol |
| `fetchpath-browser-host` | Keeps the DPAPI inbox as the durable handoff and nudges the engine so ingestion no longer waits for the desktop window | protocol |

One binary, `fetchpath.exe`, carries the engine, the commands, the TUI and the
MCP server, so the installer ships nothing new and client and engine are
always built from the same source. Split the TUI into its own crate only if
compile time or ownership justifies it.

Engine data stays in the desktop's existing folder
(`%APPDATA%\app.fetchpath.desktop`) and keeps its current file formats, so a
0.1.0 queue, history and settings load unchanged. Terminal-only preferences
(theme, aliases, keybindings, prompt history) live in the same folder in a
separate `cli.toml` and `cli-history` owned by the client, not the engine.

## 4. Protocol v1

The protocol implements the accepted [job contract](../JOB-CONTRACT.md) at the
process boundary rather than inventing a new one:

- Every message carries `schema_version`. A client and engine with different
  major versions refuse each other with `contract.unsupported_version` and the
  client explains how to restart the engine.
- Commands carry `client_id`, `command_id`, `issued_at` and optional
  `expected_revision`; the engine dedupes by `(client_id, command_id)` against
  a durable ledger and acknowledges only after the mutation commits (contract
  §5). Command names are the contract's (`CreateJob`, `Pause`, `Resume`,
  `Cancel`, `Retry`, `UpdatePolicy`, `ResolveDestination`, `SelectMedia`,
  `RefreshSource`, `RefreshMediaChoices`) plus engine-level ones: `ListJobs`,
  `GetJob`, `JobDetails`, `InspectMedia`, `InspectLink`, `QueueStats`, `History`,
  `GetSettings`, `UpdateSettings`, `Rules*`, `Approvals*`, `EngineStatus`,
  `EngineShutdown`.
- Durable events carry per-job `seq`; `SubscribeJob(after_seq)` and a
  queue-wide `SubscribeQueue(after_cursor)` replay or return a snapshot
  boundary, never an uncoordinated snapshot-then-subscribe (contract §8).
  `progress_sampled` is ephemeral and coalesced per client so a slow terminal
  cannot back up the engine.
- Errors use the contract's stable codes, `retryable` and `action` (contract
  §9). Clients render buttons, prompts and exit codes from `action` and the
  code family, never from message text.
- Framing is length-prefixed UTF-8 JSON with a hard per-message cap; malformed
  or oversize input closes that connection only.

`fetchpath-protocol` exports JSON Schema for every type. The MCP tool schemas
and the desktop's TypeScript types are generated from it, so there is one
definition of a job snapshot across the ecosystem.

## 5. Engine lifecycle

- **Single owner.** The engine claims the existing instance lock (moved from
  the desktop window). A second engine exits and its caller attaches to the
  first.
- **Launch or attach.** Every client calls one helper: connect to the pipe;
  if absent, start `fetchpath engine` detached with no console window, then
  connect with a bounded wait. Clients never host the session themselves in
  production. The in-process client exists for tests and for the TUI's
  development before the pipe lands.
- **Staying alive.** The engine runs while any client is connected or any
  job is non-terminal, including scheduled and queued jobs. With neither for a
  grace period (proposed 60 seconds), it exits. Starting at sign-in so that
  schedules survive a reboot is an opt-in setting, off by default.
- **Recovery.** Startup recovery is the existing desktop recovery, now in the
  session: pending publication intents and checkpoints are resolved before any
  client command is accepted.
- **Upgrade and uninstall.** The installer asks a running engine to stop,
  waits for it to release its files, then replaces them. An engine from an
  older build refuses a newer client by version, and the client offers to
  restart it. Uninstall stops the engine first.
- **Desktop behavior.** Closing the desktop window no longer stops downloads.
  The tray icon remains the way to reopen it; quitting from the tray quits the
  window, not the engine.

## 6. Principals, policy and approval

Until now every caller was the person at the keyboard. An agent is a different
kind of caller: web content it reads can steer it. Each connection therefore
declares a principal, and the engine, not the client, enforces that
principal's policy.

| Principal | Declared by | Allowed |
|---|---|---|
| `user` | desktop, CLI commands, TUI | Everything, as today |
| `browser` | browser host | Submit captures (with origin-scoped cookies, as today) and nothing else |
| `agent:<name>` | MCP server, one name per configured agent host | Create file and media jobs only into folders the person granted; within a size limit and a rate of new jobs; never with cookies, credentials or a `credential_ref`; never `replace_existing`; never settings, rules, LAN, cache or sharing changes; can see and control only jobs it created |

Anything an agent asks for outside its policy is not refused outright. It
becomes a job in a new `awaiting_approval` state, visible in the TUI, the
desktop queue and `fetchpath approvals`. Only a `user` principal can approve or
deny. A job whose size was unknown at creation and later crosses the agent's
limit is paused into `awaiting_approval` rather than finishing. Denial is
terminal and explained to the agent.

This adds one state to the contract's state machine: `awaiting_approval`,
entered only from creation or from `running` for the size case, and left only
by `Approve` (to `queued` or back to `running`) or `Deny` (to `cancelled`).
FP-054 amends [the job contract](../JOB-CONTRACT.md) with a superseding
decision entry and its command/state matrix rows before code changes.

**Threat model, stated honestly.** Principals defend against a misled agent
and against processes of other users or lower integrity. The pipe's access
list admits only the current user's SID, and both sides prove knowledge of a
per-install secret stored in the user's data folder, without sending the
secret itself. A malicious program already running as the same user can read
that folder and claim `user`. That is out of scope for any per-user
application, and the documentation must say so rather than imply otherwise.

**Untrusted strings.** Page titles, file names, media metadata and server
messages returned to an agent are marked as untrusted data in MCP results.
The engine makes no model calls; agents only call into it, so the rule that
no model call belongs in the download data path still holds.

## 7. Clients

### 7.1 Command line (scriptable)

All existing behavior survives: `download` keeps its arguments, output
streams, `--json`, `--quiet` and exit codes 0/2/3/4/5/6/130. New commands go
through the engine:

```
fetchpath add LINK… [--to DIR|FILE] [--sha256 HEX] [--quality Q] [--at TIME] [--wait]
fetchpath ls [--all|--active|--failed] [--json]      fetchpath show JOB [--json]
fetchpath pause|resume|cancel|retry|rm JOB…           fetchpath watch [JOB]
fetchpath inspect LINK [--json]                       fetchpath batch FILE|-
fetchpath history [QUERY]                             fetchpath settings [KEY [VALUE]]
fetchpath rules [list|add|rm|test LINK]              fetchpath approvals [approve|deny ID]
fetchpath agents [list|grant|limit|revoke]           fetchpath engine [status|stop]
fetchpath mcp                                         fetchpath        (interactive)
```

`download` becomes "add, then wait", so a scripted download now appears in the
shared queue and history. Job arguments accept an unambiguous id prefix or a
queue index. `lan`, `cache` and `fetch-verified` keep working as today; moving
them under the engine is FP-032/FP-033's business, not this plan's.

### 7.2 Interactive terminal

`fetchpath` with no arguments in a terminal opens the interactive mode.
Without a terminal, or with `--json`, it prints help and exits 2.

- **Inline mode (default).** Finished downloads print into normal scrollback
  as one-line receipts. A live panel shows active, paused, queued, scheduled
  and awaiting-approval jobs. Below it is a prompt. Pasting a link queues it
  with rules applied and shows what will happen before it starts. `/`
  commands mirror the command line (`/pause`, `/queue`, `/media`, `/rules`,
  `/theme`, `/settings`, `/approvals`, `/help`) with completion from commands,
  job names, recent folders and history.
- **Dashboard (toggle).** A full-screen view with the queue, a details pane
  (speed graph, segments and connections, reusing the desktop's details data),
  and single-key actions. Leaving it restores the inline view and scrollback.
- **Flows.** Media quality picker, batch preview, destination conflict choice,
  checksum entry and agent approval appear inline as focused prompts.
- **Accessibility.** Full keyboard use by definition. A `--plain` mode
  (automatic under `NO_COLOR`, a dumb `TERM`, or a detected screen reader
  setting) replaces redrawn panels with append-only lines. ASCII glyphs and a
  high-contrast theme are built in. Works in Windows Terminal and the classic
  console host.
- **Libraries.** `ratatui` with `crossterm` (inline viewport and alternate
  screen). FP-059 decides between a line-editor crate and a small in-house
  prompt after a short probe of Windows console behavior. Versions are checked
  against current documentation at that time.

### 7.3 Personalization

| Feature | Where it lives | Notes |
|---|---|---|
| Themes, glyph set, density, keybindings | client `cli.toml`, `/theme`, `/keys` | Built-ins include `high-contrast` and `plain`; unknown keys are reported, never fatal |
| Aliases and custom commands | client `cli.toml`, `/alias` | Expand only to Fetchpath commands; never run a shell or external program |
| Remembered context | client `cli-history`; engine history | Prompt history strips query strings and user info from links by default, because signed links are secrets; recent folders and a "since you were away" summary come from engine history |
| Smart rules | engine (session) | By domain, file type or size: destination, media quality, checksum requirement, concurrency. Applied to every principal (an agent's rule-chosen folder must still be inside its grant). `rules test LINK` explains which rule matched and why |

### 7.4 Desktop and browser

The desktop keeps its UI and journeys; only its data source changes. It gains
an approval card and an agent-access page in Settings (FP-066). The browser
host behaves as today and additionally nudges the engine, so captures reach
the queue without the window open.

### 7.5 Agents (MCP)

`fetchpath mcp` speaks MCP over stdio and is registered in an agent host's
configuration. Tools: `download` (file or media, returns a job id, optional
wait), `inspect_link`, `list_downloads`, `get_download`, `wait_for_download`,
`pause`, `resume`, `cancel`, `search_history`. Progress is reported as MCP
progress notifications. Results carry saved paths only inside granted
folders, observed or matched SHA-256 with the contract's wording (an observed
hash is never presented as publisher authenticity), and `awaiting_approval`
explanations the agent can relay. Tool schemas are generated from protocol
types. The Rust MCP SDK choice is verified against current documentation in
FP-065.

## 8. Invariants that do not change

- No overwriting at publication; no completion before validation and the
  publication barrier.
- A locally computed hash is never presented as publisher authenticity.
- Sharing, peer discovery and uploads are never enabled implicitly, and never
  by an agent.
- Secrets, cookies, signed query strings and private paths stay out of logs,
  events, prompt history, MCP results and evidence.
- No model calls in the engine.

## 9. Migration strategy

Strangler order, each step shippable on its own and checked by existing tests:

1. Pin current desktop queue behavior with characterization tests (FP-048).
2. Move it into `fetchpath-session` with the desktop calling it in-process;
   the behavior and file formats stay the same (FP-049).
3. Add the ledger and event sequencing and the protocol, in parallel
   (FP-050/051/052).
4. Stand up the engine; move the CLI onto it (FP-053/058).
5. Move the desktop and browser host onto it. Until this lands, no build given
   to users contains both an engine and an in-process desktop queue, because
   two owners of one queue is the failure this design exists to prevent
   (FP-055/056/057).

## 10. Testing strategy

- Characterization tests before the move; the same tests pass after it.
- The contract's adversarial list, now at the process boundary: duplicate
  commands, revision conflicts, event gaps and replay, slow consumers,
  malformed and oversize frames, unauthenticated and wrong-secret peers,
  client crash mid-command, engine crash around checkpoint and publication.
- Policy escape tests for agents: path traversal and symlinks out of a grant,
  size-limit crossing, credential smuggling, self-approval attempts,
  prompt-injected metadata.
- TUI render tests against recorded event streams (snapshot of the rendered
  buffer), plus plain-mode output tests; a manual Windows Terminal and console
  host walkthrough per release gate.
- The release gate (FP-069) re-runs the FP-018 matrix on the new architecture,
  including upgrade from a 0.1.0 install with a persisted queue.

## 11. Task map

Priority meanings: **P0** is on the critical path and blocks the phase; **P1**
is a headline outcome of the phase; **P2** is valuable next; **P3** is later or
research. `node tools/tasks.mjs next` lists ready tasks in this order.

| Lane | Tasks (priority) |
|---|---|
| Design | FP-047 this spec (P0) |
| Engine | FP-048 characterize (P0) → FP-049 extract session (P0) → FP-051 ledger and events (P0) → FP-053 engine host (P0) → FP-054 principals and approval (P0) |
| Protocol | FP-050 protocol types (P0, parallel with FP-048/049) → FP-052 pipe transport (P0) |
| Clients converge | FP-055 desktop client (P0) → FP-057 installer lifecycle (P0); FP-056 browser host (P1) |
| Terminal | FP-058 commands (P0); FP-059 TUI foundation (P1, starts on the in-process client after FP-051) → FP-060 dashboard, FP-061 flows, FP-062 personalization (P1) → FP-063 remembered context (P2); FP-064 smart rules (P1) |
| Agents | FP-065 MCP server (P1) → FP-066 approval surfaces and agent access (P1) → FP-067 adversarial review (P1) |
| Close | FP-068 docs and UX contract (P1) → FP-069 platform gate (P1) |
| Existing, re-sequenced | FP-032, FP-033 onto the session (P2); FP-021, FP-022 as session job kinds (P2); FP-034 (P3); FP-023–FP-025 research (P3) |

Strong-model review is required for FP-049, FP-051, FP-052, FP-053, FP-054,
FP-057, FP-065 and FP-067: persistence ordering, credential boundaries, FFI
and agent policy. Implementation plans, when needed, are scratch in ignored `work/plans`.

## 12. Open points, decided inside their tasks

- Idle grace period and the sign-in autostart setting's wording (FP-053).
- Line editor crate versus an in-house prompt (FP-059).
- MCP SDK and the exact agent-host configuration snippets (FP-065).
- Whether agents may see jobs they did not create when the person allows it
  (FP-066; default no).
