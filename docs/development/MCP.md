# Agents through MCP

FP-065 (the server), FP-066 (access and approvals) and FP-067 (the attack
on the boundary). Source: `apps/cli/src/mcp`, `apps/cli/src/agents.rs`, the
desktop's queue and Settings; the boundary itself is the engine's
(`crates/fetchpath-session`).

## What exists

`fetchpath mcp [--agent NAME]` serves the Model Context Protocol over stdio
with the official Rust SDK, `rmcp` 3.4.1 (chosen 27 September 2026 from its
crates.io record and docs: published 23 September, spec revision
2026-07-28, Apache-2.0; built without default features, with `server`,
`macros` and `transport-io`; it adds four crates, tokio being linked
already). Standard output carries only protocol messages.

- **Principal.** Every tool call connects to the engine (starting it if
  needed) as `agent:NAME` (default `agent`), so the engine enforces grants,
  size and rate holds, credential and replacement refusals, and job
  ownership exactly as for any agent client. The MCP layer only shapes what
  the agent sees. Engine calls run on tokio's blocking pool.
- **Tools.** `download` (url, folder, file_name, kind auto|file|media,
  quality, sha256, wait, timeout_seconds), `inspect_link`, `list_downloads`,
  `get_download`, `wait_for_download`, `pause`, `resume` (a failed download
  is retried), `cancel`, `search_history`. `download` goes through the same
  `queue::add_named` as `fetchpath add`: the link is looked at, a video
  page becomes a video at a quality (never the page), names are made safe.
  Auto refuses a web page unless `kind` is `file`. Without a folder it uses
  the agent's first granted folder, else sends the bare name (rules or the
  default folder decide, and the grant check makes it wait). A folder must
  be absolute; a file name must survive `safe_file_name` unchanged.
- **Schemas from protocol types.** Inputs and outputs derive `JsonSchema`
  and embed protocol types (`JobId`, `JobState`, `JobKind`, `JobFilter`,
  `ApprovalReason`, `IntegrityOutcome`, `LinkKind`, `WaitingReason`);
  schemars' non-standard numeric formats (`uint64` and so on) are stripped,
  since hosts' validators warn about them.
- **Progress.** A wait holds one connection, attach-only so it never
  restarts an engine the person stopped, polls the job every 500 ms for up
  to 120 s by default (1800 s at most, then the agent calls again), ends
  early when the job settles (finished, paused, awaiting approval, waiting
  for a source or a choice) or the call is cancelled, and sends
  `notifications/progress` with bytes and total when the caller gave a
  progress token; progress never goes backwards, and the message is
  Fetchpath's own (sizes only). At most 16 waits run at once.
- **What an agent is shown** (`view.rs`). Text from outside (file name,
  source link without query or user info, format labels, problem text,
  page titles, content types, media formats) sits under `untrusted`;
  instructions and tool descriptions say to treat it as data. A destination
  or saved path, and a job's problem text (which can name paths), appear
  only when `inside_grants` (the engine's own check, links and junctions
  resolved) places the destination in a granted folder; the agent
  reads its grants with `GetAgentPolicies`, which answers an agent with its
  own entry only (contract D3). Integrity carries the outcome, the expected
  and observed SHA-256 and a meaning that never claims the publisher.
  Awaiting approval carries the reasons and an explanation to relay. A
  tool error outside the input, policy, contract and engine families is
  labelled as untrusted detail. An agent's history search matches a
  destination's folders only inside its grant, elsewhere just the file name
  (engine, `Command::History`), so searching cannot spell out a hidden
  path.

## Access and approvals (FP-066)

- **Engine.** Changing or removing an agent's access re-checks its
  unfinished downloads (queued, scheduled, running, paused, waiting for a
  source, failed with a retry due): any whose destination is outside the
  new folders waits for approval again, a running one stopped at its
  checkpoint as a size stop is (contract D4). Revoking removes all folders.
- **Command line.** `fetchpath agents [list | grant NAME FOLDER... | revoke
  NAME [FOLDER...] | limit NAME --size S --per-hour N]` (folders must exist
  and are stored absolute; `--json` prints `AgentPolicies`), and `fetchpath
  approvals [--json]`, worded as the terminal's approval card.
- **Desktop.** A waiting request's card names its agent and why it asks,
  offers Approve and Deny (named for the download), counts under Needs
  attention, and is announced in the assertive region, also when the window
  first sees it (several at once as a count). Other agent downloads say who
  requested them. Settings has an AI agents section: add an agent by name,
  folders with Remove, Add folder (the Windows folder picker), largest
  download and downloads an hour with Save limits, and Revoke access; focus
  stays where the person was after each change.

## Verification

- `mcp::view` unit tests: paths only inside a grant, outside text only under
  `untrusted` (an injected file name, server text and format label checked
  against the rest of the JSON), hash wording, settled and approval text.
- `an_agent_reads_its_own_access_and_no_one_elses` and
  `an_agent_history_search_matches_hidden_folders_by_file_name_only`
  (`fetchpath-session/tests/policy.rs`); the view test also checks that an
  error naming a hidden path is left out.
- `apps/cli/tests/mcp.rs`: a scripted MCP client runs `fetchpath mcp` against
  a real engine: initialize (server name, tools capability, instructions),
  the nine tools each with object input and output schemas and no numeric
  formats, the protocol's `JobFilter` values in `list_downloads`;
  `inspect_link`; `download` with wait inside the grant (saved path, bytes,
  observed-hash wording, at least two progress notifications for its token
  with the total, no outside text in them); a request for another folder
  held for approval with no path shown; a link with credentials refused
  without echoing the password; a traversal file name refused; the person's
  own download invisible to list; get by id prefix, wait on a finished job,
  history search, withdrawing a request, an unknown id; a bad agent name
  exits 2 with nothing on stdout.
- Real agent host, 27 September 2026: Claude Code 2.1.283 headless
  (`claude -p`, Haiku, `--mcp-config` with only this server,
  `--strict-mcp-config`) in a scratch data folder that granted one folder to
  `claude-code`, against a local server. It called `download` with wait
  twice and saved a 300,000-byte file (bytes compared) and a small one into
  the grant, relayed the saved path, the observed SHA-256 and that it proves
  what arrived and not the publisher, and a third request into another
  folder came back awaiting approval with nothing written there. Cost
  $0.04. That run found the numeric-format warnings (fixed). The server's
  hostile `Content-Disposition` name never reached the agent: like
  `fetchpath add`, `download` names a file from its link. Re-run after the
  review fixes: same outcome, no schema warnings.
- FP-066: `revoking_an_agent_stops_its_downloads_until_the_person_approves_them`
  (narrowing holds only the removed folder's download; revoking stops a
  running 2 MiB download with nothing published; approval finishes it from
  there with the right bytes); `agents_are_granted_limited_and_revoked_from_the_command_line`
  and the `approvals` listing in
  `an_agent_request_is_approved_and_denied_from_the_command_line`
  (`apps/cli/tests/queue.rs`); the desktop view names the agent and reasons
  (`fetchpath-desktop` view tests). `tests/compatibility/windows/ui-agents.ps1`
  on the release build, 27 September 2026, in its own data folder: two
  requests made through `fetchpath mcp` waited; the queue showed "The agent
  harness asks to download this: it would save outside the folders you let
  it use." with Approve and Deny buttons named for each file; the assertive
  region said "2 agent requests wait for your approval"; Approve pressed
  from the keyboard saved all 524,288 bytes and Deny left the other
  cancelled with nothing saved; in Settings the agent name field was named,
  an agent added from the keyboard left focus on its Add folder button, a
  folder granted from the command line appeared with a named Remove, no
  focusable control in the section was unnamed, Save limits stored 5 MiB,
  Remove and Revoke access reached the engine and focus returned to the name
  field. `ui-accessibility.ps1` passed again from an empty data folder (it
  also names every control in Settings); run against a data folder with
  downloads it fails on tab order by design, since it expects the empty
  state.
- Strong review, 27 September 2026, by an independent Opus reviewer that
  derived attacks from the spec (findings and fixes in the commit message):
  no high findings; two medium path leaks (problem text, history search)
  and four low ones (progress going backwards, a connection per poll that
  could restart a stopped engine, history `total`, a double link probe)
  were fixed; tool errors quoting servers are now labelled untrusted.

## Boundary review (FP-067)

27 September 2026. An independent Opus reviewer derived attacks from the
design (section 6 and 8) and contract D1 to D4: injected names and titles,
traversal and links out of grants, size-limit games, credential and cookie
smuggling, self-approval, reading other principals' jobs, sharing and LAN,
flooding, and the D4 re-hold. The lead turned each into a test in the FP-067
section of `crates/fetchpath-session/tests/policy.rs` (with media tests in
`adapters/media` and the session, and a flood test in
`apps/cli/tests/mcp.rs`), confirmed the findings fail before their fix, and
fixed them. A second independent Opus review of the fixes found more, fixed
the same way. Contract D5 records what changed.

| Finding | Severity | Fix | Test |
|---|---|---|---|
| An agent's media download ignored its size limit: the helper's bytes were unknown until it finished | medium | The adapter measures the work folder while the helper runs, which the size stop reads | `an_agent_media_download_stops_at_its_size_limit` |
| A helper writing everything at once got past that sampling (10 of 10 runs in the second review) | medium | A hard byte cap in the adapter, set by the engine as an agent's media job starts: the helper is stopped past it and an output over it is never published; the failure becomes a `size_limit` hold | `a_burst_past_the_byte_cap_is_never_published`, `an_agent_media_burst_past_its_limit_waits_and_approval_lifts_it` |
| A folder part of a destination was unchecked: `CON`, `a:stream`, trailing dots, `\\?\` and `\\.\` paths passed, even inside a grant | low | Every part checked, only drive and share paths (`plain_path`), shared with rule folders and the default folder | `destinations_are_refused_when_any_part_is_not_a_plain_name`, `a_rule_folder_must_be_one_downloads_can_be_saved_in` |
| An agent could name a download so the approval card showed another name (`invoice` + right-to-left override + `txt.exe` read as a text file) | medium | Reordering and line-breaking characters refused in every part; joiners Persian, Indic and emoji names need are kept (the first fix refused them; the second review caught it) | `a_destination_cannot_disguise_itself_on_the_approval_card` |
| `CONIN$`, `CONOUT$` and superscript `COM`/`LPT` names were not reserved; line separators passed through names; a name cut at 200 characters could end in a dot | low | Engine, `safe_file_name` and the browser inbox share the list; separators become `_`; cut before trimming | `server_names_cannot_be_devices_break_lines_or_end_in_a_dot`, `page_chosen_names_cannot_be_devices_or_disguised` |
| `EngineStatus` showed an agent the queue's event cursor and connected clients; its job streams carried the engine-wide cursors | low | Zero for agents; an agent's stream numbers its own items | `engine_status_tells_an_agent_nothing_about_others`, `an_agent_job_stream_counts_only_its_own_events` |
| The media watcher could spin forever if the helper call panicked | low | A drop guard ends it | (code) |

Attacks that held, now regression tests: credentials in other spellings
(percent-encoded, upper-case scheme, IPv6, bare token) refused to create
and to look at; cancelling and asking again does not reset the hourly rate;
seventeen waits at once refuse one and finish sixteen, and a megabyte of
link is an error while the server carries on. Already covered by earlier
tests and confirmed by reading: the allow-list (settings, rules, access,
approvals, batches, queue streams, shutdown refused), ownership answering
`unknown_job`, the ledger keyed by principal, holds that retry and new links
cannot shed, the D4 re-hold's states and serialization, traversal and
canonical grants, create-only publication, and no HTML sinks in the desktop.

Residual risks, with reasons:

- A link or junction made inside a granted folder after a download is
  queued can carry its write outside the grant: grants are checked at
  creation, retry and access changes, not again at publication. Doing so
  needs a filesystem write as the same user, which the design places out of
  scope (an agent host with a shell already has it). Re-checking at start
  would need a persisted marker so a download the person approved outside
  the grant is not held again.
- The approval card's From line shows the link as the agent sent it (query
  and user info removed); its labels, the agent's name and the reasons are
  Fetchpath's own, so it cannot pose as an approval.
- During a video's merge the parts and the output are all in the work
  folder, so the measured size can reach about twice the file's and stop a
  video over half the limit; approval restarts it. This errs toward asking.
- Agent names are not authenticated between agent hosts (same-user scope).
- Three copies of the reserved-name list (engine, command line, browser
  inbox) agree now but are separate.

## Guided host setup (FP-102)

`apps/cli/src/agent_setup.rs` owns user-scope Codex and Claude Code registration,
with a separate `%LOCALAPPDATA%\FetchpathAgentSetup` ownership inventory.
`agent-setup add`, `remove`, `reconcile` and `cleanup` change only an explicitly
selected or still-matching owned `fetchpath` entry. Codex comments and unrelated
TOML settings survive; unrelated Claude JSON values survive. Conflicts require
explicit adoption/replacement; edited entries stop cleanup until resolved or
explicitly released with `remove HOST --keep-config`. Configuration writes use
a staged, flushed replacement and a pending ownership journal; recovery compares
the entry and prewrite digest, never stores the rest of a host's configuration.
Registration grants no agent access. `check HOST` performs a bounded stdio
handshake and tool discovery against an unchanged owned entry, without model
or download calls. Host loading policy still needs checking in the host.

The format/location sources checked on 3 October 2026 are the official
[Codex MCP reference](https://learn.chatgpt.com/docs/extend/mcp?surface=cli),
[Codex configuration basics](https://learn.chatgpt.com/docs/config-file/config-basic)
and [Claude Code MCP scopes](https://code.claude.com/docs/en/mcp).
`CODEX_HOME` and `CLAUDE_CONFIG_DIR` overrides are supported; no project
configuration is silently edited. Claude Desktop, VS Code and generic stdio
clients retain documented manual setup/removal.

The isolated registration tests are `apps/cli/tests/agent_setup.rs` (14 passed).
`tests/compatibility/windows/agent-hosts.ps1` drives registered real hosts in
scratch homes, with explicit grants and ungranted requests, and leaves only a
small result suitable for evidence. On 4 October 2026, Codex 0.160.0 loaded the
guided registration, made two actual tool calls, saved 65,536 byte-identical
fixture bytes into its grant, and left the ungranted request awaiting approval;
owned cleanup also passed. The isolated invocation explicitly approved only
the download tool in Codex and forwarded the scratch engine home: Codex filters
ambient environment variables, and `approval_mode=auto` still asks for writes.
Claude Code 2.1.289 loaded its user registration and discovered the tools, but
its saved OAuth session was expired and could not refresh. On 4 October 2026,
the owner deferred further Claude Code testing and accepted the implementation
with its model-driven download flow unvalidated. See
[host evidence](evidence/windows/agent-hosts.json).
`cargo clippy -p fetchpath --all-targets --locked -- -D warnings`, focused
formatting, and the independent boundary review and repair recheck passed.
The final explicit repository Node test list passed all 92 tests; unrestricted
test discovery also traverses copied host-plugin caches in ignored `work/`,
so those unrelated plugin suites were excluded from the repository check.
Close hosts before configuration changes: sidecar locks coordinate Fetchpath
commands, not external host writers; an external compare-to-replace race remains
unsupported. Installer
lifecycle evidence belongs in [WINDOWS-PACKAGING](WINDOWS-PACKAGING.md).

## Limitations

- The harness grants a folder from the command line rather than through
  the Windows folder picker, which UI Automation of the webview cannot
  drive. The terminal has `/approvals`, `/approve` and `/deny` but no
  `/agents`; access is changed in Settings or with `fetchpath agents`.
- Settings reads agents when it opens; a change made elsewhere meanwhile
  shows the next time.
- Each tool call other than a wait opens its own pipe connection.
- Claude Desktop and VS Code retain manual configuration examples. Guided
  Codex is verified; the latest Claude Code connection is verified, with its
  actual download unvalidated under the owner's deferral described above.
- A media format label from the helper is untrusted yet is also what the
  agent passes back as `quality`; it is only ever matched against the
  helper's own list.
