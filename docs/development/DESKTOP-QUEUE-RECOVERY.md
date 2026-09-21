# Desktop queue and recovery evidence

Verified 20 September 2026 for FP-012 on the supported Windows desktop path.

## Delivered behavior

- The Rust host owns a persistent queue and starts at most three real `fetchpath-core` jobs concurrently. The renderer never simulates progress or completion.
- The composer previews one or many destinations before enqueueing, accepts up to 100 addresses, derives sibling filenames without duplicate destinations, and can assign a future start time.
- Queue reconciliation promotes due schedules on the next poll after system wake. Persisted due items also become queued on the next application launch. Fetchpath does not wake a sleeping computer or register an operating-system wake task.
- History survives restart in the Tauri application-data directory. Search and status filters operate across the recovered history, and terminal items can be retried or removed.
- Existing destination files are never overwritten. A conflict becomes **Needs attention** with a **Choose new path** action.
- Failed, cancelled, and expired-source items expose a specific recovery action: retry, choose a new path, or paste a refreshed link.
- Closing the window hides Fetchpath to the tray. The tray's explicit quit path cancels and joins active jobs, retains their checkpoint data, requeues them for recovery, saves queue state, and exits.
- Native form controls, visible focus styling, `aria-live` status, `Ctrl+L`, `Escape`, and DOM-order tabbing provide the current keyboard path.

## Persistence and privacy boundary

Queue writes use a synced temporary JSON file, retain the previous file as `queue-v1.json.bak`, and restore from the primary file or backup when its schema is recognized. This protects against ordinary partial JSON replacement; it is not a claim of crash-proof filesystem durability under every power-loss mode.

URLs containing query or fragment values are shown redacted and are not persisted as restartable sources. After restart, those records become **Link needed** and require a refreshed address. Fetchpath currently has no credential/header persistence, encrypted secret store, or authenticated browser handoff in this queue slice.

## Verification

The optimized executable was built with:

```powershell
corepack pnpm --dir apps/desktop tauri build --no-bundle
```

`apps/desktop/tools/ui-smoke.ps1` then drove that executable through Windows UI Automation using localhost-only HTTP fixtures. It observed:

- a two-item batch preview;
- a complete 1 MiB transfer at the selected destination;
- displayed SHA-256 `bf63d8a95fcc2e64619813aae35fdcbe871fdd9264caa3f365eb3aed0f679129`, matching the file on disk;
- non-zero progress on a throttled transfer; and
- terminal cancellation with no destination file published.

The desktop Rust tests additionally exercise a real command-path download, one-slot batch concurrency, a schedule becoming due after elapsed time, safe-source recovery, private-query omission, destination conflict preservation, and user-facing invalid-input errors. The broader workspace suite retains the checkpoint, resume, cancellation, and publication-fence coverage recorded in `CHECKPOINT-RECOVERY.md`.

## Current limits

- Concurrency is fixed at three; user-configurable queue priorities, bandwidth limits, pause, and reorder are later work.
- Schedules are local timestamps and run only while Fetchpath is open or after its next launch. They do not wake the system.
- Search and filters are client-side over the loaded queue. Very large-history pagination is not implemented.
- Tray lifecycle is implemented and build-validated; this task does not claim installer, update, notification, or multi-monitor acceptance, which remains part of release work.
