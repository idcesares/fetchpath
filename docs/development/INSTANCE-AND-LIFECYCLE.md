# Instance identity, always on and the disk reserve

FP-101, the local slice (S1) of the
[instance access and remote hub design](../architecture/specs/2026-10-03-instance-access-and-remote-hub-design.md),
contract [D6](../architecture/JOB-CONTRACT.md#d6--instances-remote-principals-and-capacity-fp-091-3-october-2026)
and [D7](../architecture/JOB-CONTRACT.md#d7--automatic-mode-for-an-agent-fp-101-3-october-2026).
No network listener: remote access is FP-092, deferred to the third release.

## What was built

- **Instance identity.** `instance-v1` in the engine's data folder holds a
  random id, created once (`EngineHome::instance_id`); `instance_name` is a
  setting (blank means the computer's name, control and text-reordering
  characters removed). The pipe's `Welcome` carries the id; the pipe client
  pins the first one it reaches and stamps `expected_instance_id` on every
  command. The engine refuses a change without it, or any command naming
  another instance, with `contract.wrong_instance` (action
  `select_instance`); LAN sharing and pairing count as changes.
- **Always on** (`hub_mode`, `fetchpath hub on|off`). No idle exit, the
  sign-in start implied, and `SetThreadExecutionState(ES_SYSTEM_REQUIRED)`
  held by the engine's ticker thread only while a download runs. Not a
  service: nothing runs before sign-in.
- **Tray.** A status line (instance, downloading, waiting, approvals, always
  on) in the menu and tooltip, from `QueueStats` (now with
  `awaiting_approval`), `EngineStatus` and settings. Hide, close desktop and
  tray, and stop engine stay separate; the stop dialog names the always-on
  consequence.
- **Approval expiry.** Seven days, then `cancelled` with
  `policy.approval_expired`; an agent's retry asks again. A pre-existing
  defect was fixed on the way: a withdrawn request was restored as waiting.
- **Automatic mode for agents** (D7, added by the owner). Inside its folders
  an agent is not held for size, hourly rate or torrent peer discovery.
- **Disk reserve** (`disk_reserve_bytes`, 0 = the larger of 5 GiB and 5 %).
  Admission per drive with promised bytes of running downloads; waiting
  reason `storage_reserve` with a durable `waiting` event; running file
  downloads stopped at their checkpoint below the reserve and queued again
  by the engine's pass (`requeue_space_stopped`), never shown as cancelled.
- **Every client edits the same settings**: desktop Settings and agent
  cards, `fetchpath settings|hub|agents`, the terminal's `/settings`,
  `/hub` and `/agents` (one implementation with the CLI).

## How it was verified

- `crates/fetchpath-session/tests/instance.rs` (G1): changes must name the
  instance, reads may not name another, a rename keeps the id and survives
  a restart. `apps/cli/tests/engine.rs`: the real engine keeps its id across
  a restart, and always on keeps it up past many idle graces until turned
  off.
- `crates/fetchpath-session/tests/policy.rs`: expiry at seven days across
  restarts, withdrawn requests stay ended, automatic mode with a real
  download eight times over the size limit.
- `crates/fetchpath-session/tests/reserve.rs` (G8): on the real drive, with a
  reserve larger than any drive, a queued download waits and later
  completes, and a running one of unknown size stops, is queued and
  finishes byte-identical. `space.rs` unit tests cover the budget.
- G3, idle cost ([evidence](evidence/engine-session/g3-idle-hub.json)): an
  always-on debug engine with no clients and no downloads added 15.8 s of
  CPU in 10 minutes, because the engine ran its full pass (inbox read,
  reconcile, queue write) every 250 ms. With the pass every 5 s while idle
  (wakeups still immediate) it added 0.95 s, under the 1 s gate. Working
  set 13.8 MiB, private 1.9 MiB, 6 threads, flat; the ceiling is set at
  32 MiB working set (decision O7).
- Full workspace on 3 October 2026: 657 passed, 0 failed, 8 ignored.
- An always-on engine notices time passing (approval expiry, a newly due
  scheduled download, an old browser host's capture) within 5 s while idle.

- FP-103 G2, final 0.2.0 installer: actual guest sign-out/sign-in in Windows
  Sandbox 26100. The engine survived forced desktop exit, started automatically
  before any client could launch it after a new logon, kept its instance and
  executable identity, resumed from byte 294912, and verified all 2097152 bytes.
  The same automatically started process stayed up beyond idle grace.
  [Evidence](evidence/windows/sign-in-lifecycle.json).

## Limitations that still hold

- G2 proves actual Windows guest sign-out/sign-in on the default data folder.
  A physical-machine reboot and sudden power loss remain untested.
- **G3 passes narrowly** on one debug-build run; a release build has not
  been measured. The idle pass still rewrites the queue file every 5 s
  when nothing changed.
- The reserve stops only file downloads while running; video, audio and
  torrent downloads are checked when they start. A drive that cannot be
  measured holds nothing back. Ranged writes can make the accounting
  slightly conservative, never optimistic.
- Consequential actions name the instance in the tray and status only; the
  add and approval surfaces name it once a second instance can be selected
  (FP-092).
- Per-principal active and submission limits moved to FP-092.
- Independent lifecycle and security reviews ran for FP-103. Repairs keep the
  desktop pinned to its original instance through reconnects, restart running
  helper jobs when automatic policy changes, and retain torrent recovery
  identity until completion is durable. The independent final publication
  repair recheck passed; the release gate records the evidence.
