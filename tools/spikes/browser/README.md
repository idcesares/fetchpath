# Browser handoff spike (FP-006)

This spike uses an isolated Chrome profile, a localhost fixture server, a Manifest V3 extension, and a real Windows native-messaging host. The host writes only redacted capture metadata and calls `sync_all` before acknowledging an accepted handoff. The extension cancels an eligible browser download only after that acknowledgement; rejected or unavailable handoffs stay in the browser.

Run from the repository root:

```powershell
powershell -ExecutionPolicy Bypass -File tools/spikes/browser/run-browser-spike.ps1
```

The runner builds the Rust host in `work/browser-spike/`, acquires the official stable Chrome for Testing archive into that ignored work area when absent, temporarily registers only `HKCU:\Software\Google\Chrome\NativeMessagingHosts\com.fetchpath.browser_spike`, restores any prior value in `finally`, and writes redacted evidence to `docs/development/evidence/browser/browser-spike.json`. It never opens the user's normal Chrome profile. Chrome for Testing is used because branded Chrome removed the `--load-extension` switch in version 137.

The tracked Firefox manifest demonstrates the distinct add-on identity and permission shape. Firefox and Edge runtime execution remain separate environment gates; see `docs/development/BROWSER-HANDOFF-SPIKE.md`.
