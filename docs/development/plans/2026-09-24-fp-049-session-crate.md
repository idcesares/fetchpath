# FP-049: move the queue into `fetchpath-session`

**Goal:** Move the desktop queue into `crates/fetchpath-session` with no behavior change, so the engine (FP-053) and every client can share it. The desktop calls it in-process.

**Constraints:** File formats, file names and the data folder stay the same. The FP-048 characterization tests move with the code and pass unchanged apart from the type name. No Tauri or UI types in the session. Strong-model review of persistence ordering before done.

## Boundary

The desktop `lib.rs` already split cleanly: lines 21–1995 (queue, persistence, retry, rates, media jobs, validation) use no Tauri types, and everything after them is Tauri commands, tray, the single-instance guard and `run`.

| Goes to the session | Stays in the desktop |
|---|---|
| `DesktopJobs` → `Session`, `QueueRecord`, `RateEstimate`, persistence, validation, media orchestration | Tauri commands, `SettingsView` (needs the Windows Downloads folder from Tauri), tray, `single_instance`, `run` |
| `settings.rs` | `media_setup.rs` (guided helper install; a setup surface, not queue behavior) |
| The browser inbox store, capture validation and DPAPI from `browser_bridge.rs` (the queue reads the inbox and its secrets) | The native messaging host loop, caller check and framing from `browser_bridge.rs`; `browser_setup.rs` |
| The queue tests, the characterization tests and their fixtures | The single-instance test and the host framing test |

## Steps

1. Record a baseline: `cargo test -p fetchpath-desktop --locked`, and the sorted test names from `-- --list`.
2. Slice the files mechanically at the lines above, so the moved code is byte-identical; `git mv` the settings, characterization and fixtures.
3. Make public only what the desktop calls; replace the two direct field reads (`settings_repaired`, `link_reviews`) with accessors.
4. Add the crate to the workspace, depend on it from the desktop, drop the desktop dependencies that no longer have a user.
5. Diff the moved code against the original to confirm only visibility, accessors, paths and the rename changed; compare the test lists.
6. Run the workspace tests, clippy, fmt, `node --test` and the backlog check; update the repository map, the FP-048 evidence paths and [the engine session record](../ENGINE-SESSION.md).
7. Desktop walkthrough before and after, and the strong review, before FP-049 is done.
