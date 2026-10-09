# Instance access and an optional remote download hub

Status: accepted by the user · 3 October 2026 · remote slices S2 to S4 and the required strong review deferred by the user to the third release (after 0.2.0) · local web UI slice S1b (§17) proposed 8 October 2026: direction accepted by the user, decisions O9 to O13 settled (§15) · Task FP-091, S1b FP-104 · Milestone M15 ·
Acceptance ids A01, A09, A13. Extends [the engine platform design](2026-09-24-engine-platform-design.md)
and [the job contract](../JOB-CONTRACT.md) (D1 to D5). No deployment or
network exposure is part of this task.

## 1. Purpose and limits

One engine already owns one queue and every surface is its client (platform
design §3). This design lets those clients, plus a browser and an agent on
another device, select one engine explicitly, local or remote, and act on that
engine's queue without any surface keeping its own queue.

Two rules bound everything below:

- **Remote serving is opt-in and off by default.** Nothing listens beyond the
  per-user pipe until the person turns hub mode on at the host. The one
  exception is the local web UI (§17): a loopback-only listener, also off by
  default, that the local user turns on separately and that serves no other
  device.
- **Control and file delivery are separate.** The control plane carries
  commands, events and snapshots. It never carries file bytes. Retrieving a
  completed file is its own grant, slice and listener route.

Not in scope: a cloud relay or account service, a Windows service, running
before sign-in, multi-user hosts, and automatic delivery (designed in FP-093).

## 2. Delivery slices and their gates

Each slice ships alone and is gated before the next starts.

| Slice | Delivers | Task (proposed) |
|---|---|---|
| S1 Local usability | Instance identity and name, hub mode lifecycle, tray as management client, capacity limits, wrong-host guard in the protocol | FP-101 |
| S1b Local web UI | Loopback listener at `fetchpath.localhost`, launch-ticket sign-in, the browser client (queue, add, details) on this PC only (§17) | FP-104 |
| S2 Remote control | Remote listener behind a private gateway, device enrollment, credential-bound principals and grants, minimal browser client (queue, add, details, approvals), remote agent adapter | FP-092 |
| S3 Completed-file retrieval | `retrieve` grant, single-use retrieval tickets for published outputs | FP-093 (detail design), then its implementation packet |
| S4 Automatic delivery | Optional delivery of completed outputs to a chosen device | FP-093 (design only until S3 passes) |

Gates are in §13.

## 3. Instance identity and selection

**Identity.** At first start an engine creates `instance.json` in its data
folder (`%APPDATA%\app.fetchpath.desktop`): a random `instance_id` (opaque
UUID, contract §2) and a person-assigned `name` (default: the computer name).
The id never changes for that data folder; the name can. Remote enrollment
additionally pins the host's long-lived ed25519 key, the DPAPI-protected device
identity `adapters/lan` already keeps, so a remote client authenticates the
host by key, not by address. `EngineStatus` gains `instance { id, name }`.

**Selection is explicit.** Clients keep a list of known instances in client
configuration (`cli.toml`, desktop settings, the browser client's origin):
name, pinned `instance_id`, pinned host key fingerprint for remote entries,
endpoint, and a reference to its DPAPI-protected credential. The local engine
is the default. A client never falls back from an unreachable remote instance
to the local one, or the reverse; it reports the selected instance as
unreachable.

**Wrong-host guard.** Every mutating command carries `expected_instance_id`.
An engine that is not that instance refuses with
`contract.wrong_instance` (not retryable, action `select_instance`). Local
clients send it too, so a data folder swapped under a running client is caught.
Consequential actions (add, cancel, remove, approve, deny, retrieve, stop
engine) display the instance name and the host-owned destination folder in
the confirming surface, for example "Save on **Studio PC** in
`Downloads\Datasets`".

**Paths belong to the host.** Destinations, the default folder and rule
folders are host paths. A remote client never maps them to its own disk and
never browses the host's disk. It picks from host-published folder choices: the
default folder, rule folders and, for agents, granted folders, named by a label
plus a relative path (`ListFolderChoices`, new, read only). Full host paths are
shown to the local user and to remote person devices holding the `view_paths`
option (§6); everyone else sees the label form, the same redaction agents get
today (D3).

## 4. Version negotiation

The protocol keeps `schema_version` and the major-version refusal. The remote
handshake adds `features`, a list of named capabilities the engine supports
(`approvals`, `retrieve`, `folder_choices`, ...). Clients hide what the engine
lacks rather than sending commands to learn. A remote client facing a newer
or older major version shows "Update Fetchpath on *name*", because it cannot
restart a remote engine the way a local client does.

## 5. Transport and network exposure

### Options compared

| Option | Exposure | Verdict |
|---|---|---|
| A. Private-network gateway: the engine listens on loopback only; a gateway on the host (Tailscale `serve` is the candidate) proxies authenticated tailnet traffic to it with HTTPS | Only devices in the person's private network reach the gateway | **First slice** |
| B. Bind the engine to the private-network interface address directly | Same network reach; the engine then handles TLS and interface changes itself | Fallback if A's proxy behavior fails a gate |
| C. Direct public exposure (port forward, own TLS, public DNS) | Internet-wide scanning, credential stuffing, DoS, certificate operations | Rejected for now; revisit only with its own design and review |
| D. A Fetchpath relay service | A hosted service and accounts | Out of scope |

Option A keeps the engine's listening surface at loopback. Fetchpath ships no
gateway and depends on none: any private-network tool that proxies HTTPS to a
loopback port works, and the documentation names Tailscale as the tested one.
Gateway identity headers (such as Tailscale's user login header) are advisory
display data only, because any local process can call the loopback port
directly and forge them. Authentication is always Fetchpath's own (§6). The
exact `tailscale serve` behavior (TLS termination, header forwarding,
WebSocket support, what happens when the tailnet drops) is checked against
current Tailscale documentation and a real tailnet in S2's first gate.

### Listener

A remote module in the engine (`apps/cli/src/remote`, planned) serves, on one
loopback port chosen at hub-mode setup:

- `GET /` and static assets: the browser client, embedded in `fetchpath.exe`.
- `POST /enroll`: exchanges a pairing code for a device credential (§6).
- `GET /v1/socket`: a WebSocket carrying protocol v1 frames (same JSON
  envelope, same per-message cap, same `contract.*` close rules).
- S3 only: `GET /v1/retrieve/<ticket>`.

Every remote connection enters the existing `Engine` exactly as a pipe
connection does, with a principal fixed by its credential. No queue, ledger
or event logic is duplicated.

### Remote clients

- **Browser client.** Served by the listener; queue, add, job details and
  approvals only (FP-092). It uses the design system tokens and needs no
  agent.
- **Agent adapter.** `fetchpath mcp --instance NAME` stays a local stdio MCP
  server for the agent host, and speaks to the selected remote instance over
  `/v1/socket` with an agent credential instead of over the pipe. The MCP
  tools, schemas and untrusted-data marking are unchanged; results name the
  instance. No MCP-over-HTTP endpoint is exposed by the engine.
- **Remote CLI and desktop.** The same socket client behind `EngineClient`,
  selected through the instance list (§3).

## 6. Principals, credentials and grants

**Principals are bound, not declared.** On the pipe a client declares its
principal (D1); that is acceptable only because the pipe admits one user and
a shared secret. Remote connections have no declaration field: the engine
derives the principal from the credential. Two new principal kinds:

| Principal | Who | Grants possible |
|---|---|---|
| `device:<id>` | A person's enrolled device (browser or remote CLI/desktop) | `view`, `submit`, `approve`, `retrieve`; option `view_paths` |
| `agent:<name>@<device>` | An agent enrolled for remote use | `view` (own jobs only) and `submit`, under D1 agent policy; never `approve` |

**Grants are separate and minimal.**

- `view`: list jobs, job details, subscribe to events. For an agent, own jobs
  only (D1).
- `submit`: `CreateJob` into host folder choices, plus pause, resume, cancel,
  retry on jobs the principal may view. A person device submits with person
  policy except that cookies, `credential_ref` and `replace_existing` stay
  local-only in S2 (open decision O6).
- `approve`: `ApproveJob`, `DenyJob`, list approvals. Only person principals.
  The engine refuses to grant it to an agent, and refuses an approval whose
  approving principal is the job's requesting principal (`policy.self_approval`).
- `retrieve` (S3): obtain a retrieval ticket for a published output the
  principal may view.

Settings, rules, agent access, enrollment, revocation of others, LAN,
sharing, cache, hub mode and `EngineShutdown` stay local `user` only. A
remote principal can sign itself out and nothing more about access.

**Enrollment.** At the host the person runs `fetchpath remote pair` (or the
desktop's Remote access page), chooses the grants and a device label, and gets
a single-use code valid for two minutes with at least 40 bits of entropy (the
FP-020 pairing pattern and its stated offline-guess limitation). The device
submits the code to `/enroll` and receives a 256-bit random credential plus
the host key fingerprint to pin. The host stores only a hash of the
credential. Five wrong codes invalidate the current code. A browser keeps its
credential in an `HttpOnly; Secure; SameSite=Strict` cookie; a remote CLI or
agent adapter keeps it DPAPI-protected.

**Revocation.** The person revokes a device at the host. Its live
connections close at once, its credential stops matching, and a command from
it that has not committed by then is refused. Jobs it created stay in the
queue as the person's jobs; an agent's unfinished jobs follow D4 (they wait
for approval). Credentials unused for a configurable period expire (O2).

**Ledger identity.** The command fingerprint already includes non-`user`
principals (D1). A remote client's `client_id` is derived from its device id,
so a command retried after reconnect deduplicates against the same ledger
entry, and a reused id from another principal is an idempotency conflict.

## 7. Browser-origin protections

- The browser client and the API share one origin. No CORS headers are sent.
- Every request and the WebSocket upgrade must carry a `Host` in the
  configured allowlist (the gateway hostname) and, when present, a matching
  `Origin`. This defeats DNS rebinding against the loopback port.
- State changes travel only over the authenticated WebSocket, whose upgrade
  requires the session cookie and an `Origin` check; there are no
  cookie-authenticated state-changing GET or form routes.
- `Content-Security-Policy: default-src 'self'; frame-ancestors 'none'`, no
  third-party scripts, fonts or analytics, `Referrer-Policy: no-referrer`.
- Page titles, file names, media metadata and server messages are rendered as
  text, never markup, and are marked untrusted for agents (platform design §6).

## 8. Redaction

Remote principals receive the contract's public payload (§8 of the contract):
source display values without user info or query strings, and destinations in
label form unless `view_paths`. Credentials, cookies, pairing codes, retrieval
tickets and the host key's private half never enter logs, events, history or
evidence. Remote access logs record device id, principal, command name,
outcome and time only.

## 9. Connection loss, replay and engine lifetime

- **Downloads continue on disconnect.** A client connection never owns work
  (already true for the pipe).
- **Reconnect replays.** A reconnecting client resubscribes with
  `SubscribeQueue(after_cursor)` or `SubscribeJob(after_seq)` and gets a replay
  or a snapshot boundary (contract §8). A slow remote consumer is cut with
  `resource.subscriber_lagging`, as locally.
- **Retried commands are idempotent.** The browser client keeps unanswered
  `command_id`s in session storage and resends the same envelope after
  reconnect; the ledger returns the stored result.
- **Client-side states.** A remote client distinguishes *connected*,
  *reconnecting* (lost within the last 30 s), *host sleeping* (the host sent a
  `host_suspending` notice on its way to sleep, with the time) and
  *unreachable, cause unknown* (with last-seen time). It never shows "offline"
  or "online" for a host it cannot reach, because it cannot tell sleep,
  shutdown and network loss apart.

## 10. Hub mode lifecycle

Hub mode is one host setting, off by default, set only by the local user.

| | Ordinary engine (today) | Hub mode on |
|---|---|---|
| Idle exit | After 60 s with no clients and no work | Never while hub mode is on |
| Remote listener | None | Loopback listener for the gateway |
| Sign-in start | Optional (`start_engine_at_sign_in`) | Required and turned on with hub mode |
| After reboot | Starts at sign-in if chosen | Starts at sign-in, recovers persisted jobs (existing startup recovery), then resumes serving |
| Desktop exits | Engine continues while it has work | Engine continues serving |

**Supported lifecycle, stated plainly.** The engine runs in the person's
Windows session. After a restart nothing runs until that person signs in; hub
mode is not a system service and the documentation says so. A host that went
to sleep is unreachable until it wakes. While jobs are running, hub mode
keeps the computer awake (`SetThreadExecutionState` with
`ES_SYSTEM_REQUIRED`, released when no job is active); keeping an idle hub
awake is the person's power setting, not Fetchpath's (O4).

**Idle cost.** An idle hub must not poll: the accept loop and ticker wait on
events. Budgets are a gate (§13).

## 11. Tray and headless management

The engine owns downloads; the tray is a management client of it.

The tray shows: engine state (running, stopped, read-only queue), active work
(count and combined rate), remote access (off, serving as *name*, or the
listener's error) and pending approvals (count, opening the approvals view).

Its actions are separate and labelled by scope:

| Action | Effect |
|---|---|
| Show Fetchpath | Opens the window |
| Close window (window's close button with close-to-tray) | Hides the window; tray stays |
| Close desktop and tray (downloads continue) | Exits the desktop; engine and hub keep running |
| Turn off remote access | Turns hub mode off: closes remote connections and the listener; engine keeps downloads and returns to ordinary idle exit |
| Stop engine (stops downloads)… | Confirms first; with hub mode on the confirmation adds "Remote devices lose access until Fetchpath starts again" |

Headless operation needs no tray: `fetchpath hub on|off|status`,
`fetchpath remote pair|devices|revoke DEVICE`, `fetchpath engine status|stop`,
and `fetchpath mcp` (local, unchanged). `engine status` reports instance name,
hub mode, listener state and connected remote devices.

## 12. Capacity, retention and approvals

**Engine-owned capacity limits** (local `user` sets them):

- `min_free_bytes` per destination volume (O5 sets the default). A job whose
  remaining expected bytes would cross the reserve waits as
  `waiting(storage.reserve)` instead of failing; a job of unknown size is
  checked at each progress sample and paused at its checkpoint at the reserve.
- Staged-byte accounting: the sum of remaining expected bytes of non-terminal
  jobs on a volume counts against free space before admitting another job, so
  two large jobs cannot each pass a check that only one can satisfy.
- `max_active_downloads` (exists) plus per-principal limits on active jobs,
  pending approvals (exists for agents) and submission rate for remote
  devices.

**Retention.** History length and age stay host settings. Completed files are
never deleted by the engine unless the person sets an explicit deletion policy;
removing a job removes the record, not the file, for every principal.
Retrieval (S3) never implies permission to delete.

**Approvals.** Requests come from D1 rules. A request now also expires after a
configurable period (O3): it becomes `cancelled` with
`policy.approval_expired`, which the agent can relay. Pending, denied and
expired are visible to the requester with reasons. Within pre-granted folders,
size and rate an agent's job runs unattended, as today. The approval card on
every surface shows requester, instance, destination label, size and reasons.

## 13. Implementation gates

| Gate | Measurable condition |
|---|---|
| G1 Identity | `instance_id` stable across engine restart and upgrade; rename changes only `name`; every mutating command without the right `expected_instance_id` is refused in protocol tests |
| G2 Lifecycle | Hub mode survives desktop exit and sign-out/sign-in reboot on a real Windows 11 machine; persisted jobs resume; ordinary mode still exits after the idle grace |
| G3 Idle budget | Idle hub over 10 minutes: no busy polling (CPU time added under 1 s), working set recorded and under a stated ceiling (O7) |
| G4 Gateway | Tailscale `serve` (or option B) checked against current docs and a real tailnet: TLS, WebSocket, Host header, behavior on tailnet loss |
| G5 Auth | Tests: no credential, wrong credential, revoked credential mid-session, expired code, reused code, five wrong codes, forged gateway headers on loopback, wrong `Host`, cross-origin upgrade |
| G6 Grants | One test per grant proving each command family is allowed only with it; agent cannot hold `approve`; self-approval refused |
| G7 Replay | Disconnect mid-transfer: download completes; reconnect replays without gaps or duplicates; retried command returns the stored result |
| G8 Capacity | Fill a test volume to the reserve: jobs wait, nothing fails or publishes partially; two concurrent large jobs respect staged accounting |
| G9 Review | Independent strong review of authentication, authorization, replay and browser boundaries before S2 ships; SECURITY.md scope updated in the same change that adds the listener |

## 14. Walkthroughs

1. **One home queue, three remote surfaces.** The desktop on the host, a
   browser on a laptop and an agent adapter on the laptop all select *Studio
   PC*. The agent submits into its granted folder and runs unattended; it
   submits outside its grant and the request appears in the browser's
   approvals and the desktop's approval card; the person approves from the
   browser; all three see the same `job_id`, sequence and state.
2. **Wrong host.** The laptop has *Studio PC* and *Laptop* (local). The person
   selects *Laptop*, then the network drops and the laptop's engine answers
   instead: it refuses commands carrying *Studio PC*'s `expected_instance_id`.
3. **Idle serving.** No jobs, desktop closed: the hub keeps the listener open
   within the G3 budget; the browser connects and adds a link.
4. **Reboot.** The host restarts mid-download. Until the person signs in the
   browser shows "unreachable since 14:02, cause unknown". After sign-in the
   engine recovers the checkpoint and resumes; the browser reconnects and
   replays.
5. **Disconnect during transfer.** The laptop sleeps; the download continues
   and publishes; on wake the browser replays `publication_completed`.
6. **Sleeping host.** The host sleeps with no active job and sends
   `host_suspending`; the browser shows "Studio PC went to sleep at 23:10".
7. **Full disk.** A 40 GB submission on a volume with 30 GB free above the
   reserve waits as `storage.reserve`; the browser shows why and the person
   frees space or picks another folder choice.
8. **Close to tray, Quit, Stop.** Closing the window hides it; "Close desktop
   and tray" leaves downloads and remote access running; "Stop engine" asks,
   names the remote consequence, then stops.
9. **Headless.** No desktop installed: `fetchpath hub on`, `fetchpath remote
   pair`, `fetchpath engine status` and `fetchpath mcp` cover the same
   lifecycle.

## 15. Decisions (accepted by the user, 3 October 2026)

| # | Question | Decision |
|---|---|---|
| O1 | Gateway approach for S2 | A: loopback plus `tailscale serve`, B as fallback |
| O2 | Idle expiry of device credentials | 90 days unused; revocation is always immediate |
| O3 | Approval request expiry | 7 days |
| O4 | Keep the host awake while idle in hub mode | No; only while jobs run |
| O5 | Default disk reserve | The larger of 5 GiB and 5 % of the volume |
| O6 | May a remote person device use cookies, stored credentials or replace existing files | Not in S2 |
| O7 | Idle hub working-set ceiling | Measure first in G3, then fix the number |
| O8 | Browser client authentication beyond the device credential (passkey) | Defer; revisit before any option C |

Accepted by the user on 8 October 2026 for S1b: build a local web UI first,
on this PC only, at a `*.localhost` name; drop `.local` (mDNS) names, which
need a LAN listener and have no trusted HTTPS. O9 was chosen by the user;
O10 to O13 were settled by the lead at the user's request, the same day:

| # | Question | Decision |
|---|---|---|
| O9 | HTTP and WebSocket server crate (a new dependency; tokio has no `net` feature today) | `hyper` 1 (`server`, `http1`) with `hyper-util`, `http-body-util` and `tokio-tungstenite` 0.30 (`handshake` only), plus tokio `net`; no framework, our own routing. A scratch stub (loopback HTTP/1, one static route, WebSocket echo) measured +590 KiB in release against an 11.8 MB `fetchpath.exe`, and 10 crates new by name to its tree. OSV (RustSec) showed no advisory for them at the resolved versions |
| O10 | Port | Default 47474: below the Windows dynamic range (49152 and up) and clear of WinRM's local 47001. If it is taken, try the next 9 ports, then let the OS choose. The engine remembers the last port it bound and prefers it next time, so bookmarks keep working. The launcher always opens the port the engine reports, never an assumed one. Settings shows the current address |
| O11 | Browser session lifetime | Browser-session cookie; the engine keeps only its hash, in memory, so every session ends when the engine stops (owner, 8 October 2026: a process holding the port while the engine is down must not collect a cookie that still works). It also ends after 30 days unused, on "Sign out all browsers" (which also voids live tickets) and when the web UI turns off |
| O12 | Approvals in the local web UI | Not in S1b (§17). A job waiting for approval shows the reason and "Approve it in Fetchpath on this PC". Revisit with the G9 review |
| O13 | Reveal a completed file in Explorer from the web UI | Yes, since the viewer is on the host. It is limited to published outputs of jobs the session may view, and no file bytes are served |

## 16. Contract changes this design implies

Applied to [the job contract](../JOB-CONTRACT.md) as [D6](../JOB-CONTRACT.md#d6--instances-remote-principals-and-capacity-fp-091-3-october-2026), before code:
`instance` in `EngineStatus`,
`expected_instance_id` and `contract.wrong_instance`; credential-bound
`device:` and remote `agent:` principals with the four grants;
`policy.self_approval`, `policy.approval_expired`; `waiting(storage.reserve)`;
`ListFolderChoices`; `host_suspending` as an ephemeral notice; the `features`
list in the remote handshake.

## 17. Local web UI (slice S1b, FP-104)

**Purpose.** The same queue, in a browser tab on the host, with the desktop's
look. It is the S2 browser client with a local sign-in instead of enrollment,
so S2 later adds a transport and enrollment, not a second client. No other
device can reach it.

**Listener.** The remote module (`apps/cli/src/remote`) starts, and S2 extends it.
When the local user turns the web UI on (desktop Settings or `fetchpath web
on`; off by default), the engine binds loopback only, `127.0.0.1` and `[::1]`,
on the port from O10 (default 47474). Routes are those of §5 minus `/enroll`: `GET /` with
static assets embedded in `fetchpath.exe`, `GET /ui/socket` carrying the
client view API (below), and `GET /open` (below). The raw protocol v1 socket
of §5 is not served in S1b; it arrives with S2 for the remote CLI and agent
adapter. The address shown to the person is
`http://fetchpath.localhost:PORT`. Turning the web UI off closes the port and
every browser session. It runs whether or not the desktop is open, and it
does not keep the engine alive on its own (§10 still decides that).

**Sign-in.** Loopback is not authentication: any local process, and any web
page through the browser, can reach the port.

1. "Open in browser" (desktop, tray, `fetchpath web open`) asks the engine
   over the authenticated pipe for a launch ticket: 256 random bits,
   single use, valid for 60 seconds.
2. The launcher opens `http://fetchpath.localhost:PORT/open?ticket=…`.
3. The engine consumes the ticket, sets a session cookie (`HttpOnly;
   SameSite=Strict`, `Secure` where the browser accepts it on localhost),
   and redirects to `/` so the ticket leaves the address bar and history.
4. Without a valid cookie, `/` shows only "Open Fetchpath from the tray or
   desktop to sign in", and the socket upgrade is refused.

Tickets and cookies never enter logs, events or evidence (§8). A process that
binds the port before the engine would receive a ticket. The launcher opens
only the port the engine reports it bound, and the G9 review examines what
remains.

**Principal and grants.** A browser session is a `device:<id>` principal (D6)
created by the launch ticket, with `view` and `submit` only, under person
policy with the O6 limits (no cookies, stored credentials or replacing
existing files). It has no `approve`. A browser-driving agent on this PC could
otherwise approve its own pending jobs and bypass D1/D4. Approvals stay in
the desktop and tray (O12). Settings, rules, agent access, LAN, cache, hub
mode and shutdown stay desktop and CLI only, as in §6. Decided with the owner
on 8 October 2026 (rules in JOB-CONTRACT D6): one device id per sign-in; the
device adds file downloads only, into folder choices shown with host paths;
it cannot move a job, change its link or checksum, or retry a denied,
withdrawn or expired request, but may withdraw a pending request by
cancelling or removing it; a `device:` job makes the store unreadable by
0.2.0, which the release notes state. The residual risk, an
agent driving the browser to submit as the person, is recorded for the G9
review, with these: a local server on another port of `fetchpath.localhost`
receives the cookie (cookies ignore ports; it is set on that name only, never
on `localhost` or `127.0.0.1`); a call already running finishes after
sign-out; and `Secure` is not set until G10 shows each browser keeps it on
http loopback.

**Browser protections.** §7 applies unchanged. The `Host` allowlist is
`fetchpath.localhost:PORT`, `localhost:PORT`, `127.0.0.1:PORT` and
`[::1]:PORT`, with a matching `Origin` on the upgrade. No CORS headers, the
same CSP, and untrusted text rendered as text.

**One client, two transports.** The desktop's `main.ts` calls Tauri commands
directly. S1b introduces an engine API interface (`apps/desktop/src/engine`)
with a Tauri implementation and a socket implementation, and moves the queue,
add and details views and
`styles.css` into modules both entries load. The desktop keeps every screen.
The web entry includes only the S1b scope: queue, add into host folder choices
(`ListFolderChoices`), job details, pause, resume, cancel, retry, remove, and
reveal on the host (O13). FP-094 tokens and patterns apply: responsive,
keyboard and focus accessible, light, dark and high contrast. The web build
is a second Vite entry in `apps/desktop`. The release build produces it
before `fetchpath.exe` embeds it; a development build without it serves a
"web UI not built" page.

**Client view API (decided by the user, 8 October 2026).** The browser
receives the same JSON the desktop's Tauri commands return, so job state
words, available actions and figures come from one mapping. That mapping (job
view, queue statistics, job details and the job draft conversion) moves from
`apps/desktop/src-tauri/src/view.rs` to a `view` module in
`crates/fetchpath-protocol`, which the desktop re-exports and the listener
uses. `/ui/socket` accepts only a fixed list of named calls, the `EngineApi`
methods in the S1b scope, each as `{id, call, args}` answered by `{id, ok}`
or `{id, error}`, plus `queue` and `engine` change notices. Anything else
closes the socket. The listener executes each call against `Engine` as the
session's `device:` principal, so grants are enforced by the engine, not by
the list. A short fixed call list is also what the G9 review reads, instead
of all of protocol v1 behind grant filtering.

**Gate G10 (S1b).**
- On a real Windows 11 machine, `fetchpath.localhost` resolves to loopback
  in current Chrome, Edge and Firefox, whichever of IPv4 or IPv6 each picks.
  The page is a secure context, and the cookie attributes hold.
- Tests cover: no cookie, an expired, reused or forged ticket, a wrong
  `Host` (DNS rebinding), a cross-origin upgrade, a refused `approve` and a
  refused settings command from a browser session, and web UI off closing
  live sockets.
- The port is unreachable from another interface.
- G7's disconnect and replay checks pass over the socket.
- SECURITY.md scope is updated in the same change that adds the listener, and
  an independent strong review covers the sign-in, grants and browser
  boundary before S1b ships.
