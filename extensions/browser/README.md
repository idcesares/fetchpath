# Fetchpath browser extension

This is the production browser-control surface for FP-013. It uses an explicit **Send link to Fetchpath** context-menu action; it does not watch, cancel, or replace ordinary browser downloads. Unsupported URLs, denied site permissions, excluded origins, and unavailable native hosts therefore leave the browser path untouched.

On the first send for an origin, the browser asks for that exact HTTP/HTTPS host permission. Applicable cookies are sent only through native messaging after the user gesture. The Windows host validates their domain/path/secure scope, protects the source URL, referrer, and cookie records with current-user DPAPI, commits a durable idempotent inbox record, and only then acknowledges. Routine extension events and inbox JSON contain a query-free URL and cookie count, never cookie names, values, or signed query values.

`manifest.chromium.json` has the fixed unpacked-extension key expected by the native host and covers Chrome and Edge API behavior. `manifest.firefox.json` carries Firefox's distinct add-on identity and background declaration. The native-host manifest templates are materialized by packaging with the absolute installed path of `fetchpath-browser-host.exe`; browser-specific registry registration remains installer work.

Run the policy and native tests from the repository root:

```powershell
node --test extensions/browser/test/*.test.mjs
cargo test -p fetchpath-desktop --locked
```

The real Chromium check uses the cached official Chrome for Testing build from FP-006, an isolated profile, a temporary current-user native-host registration that is restored in `finally`, and localhost-only fixtures:

```powershell
pwsh -NoProfile -File extensions/browser/test/run-chromium-runtime.ps1
```
