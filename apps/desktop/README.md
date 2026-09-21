# Fetchpath desktop

The desktop shell is a Tauri 2 application. Its webview owns presentation only. The Rust host owns a persistent queue of real `fetchpath-core` `FileJob` instances and reports schedules, progress, cancellation, recovery actions, terminal state, destination, and the observed SHA-256 digest back to the renderer.

The queue runs at most three downloads concurrently. It supports batches of up to 100 items, future start times, start-now, search and status filters, retry, destination-conflict recovery, removal from history, and orderly checkpoint retention on quit. Schedules that become due while Windows is asleep are caught up by the next queue reconciliation after wake. They do not wake the computer or launch Fetchpath when the app is not running.

Queue history is stored under the Tauri application-data directory as `queue-v1.json` with a last-known-good backup. Addresses containing query or fragment values are redacted in history and are never stored for restart; Fetchpath asks for a refreshed link instead. This avoids writing common signed-link secrets to disk.

## Develop and verify

Run these commands from the repository root:

```powershell
corepack pnpm --dir apps/desktop install --frozen-lockfile
corepack pnpm --dir apps/desktop build
cargo test -p fetchpath-desktop
corepack pnpm --dir apps/desktop tauri build --no-bundle
pwsh -NoProfile -File apps/desktop/tools/ui-smoke.ps1
```

The UI smoke test drives the optimized Windows application through UI Automation against localhost-only HTTP fixtures. It verifies a two-item batch preview, a completed 1 MiB download and matching observed digest, then cancels a throttled transfer and verifies that no destination file is published. Test downloads stay under `work/desktop-e2e`.

Keyboard operation follows native form order. `Ctrl+L` selects the address field and `Esc` clears an unsubmitted composer or recovery edit and returns focus to **Add to queue**.

Closing the main window hides it to the notification area. Use **Show Fetchpath** to reopen it and **Quit Fetchpath** for an orderly shutdown that cancels and joins active jobs.
