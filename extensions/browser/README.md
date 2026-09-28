# Fetchpath browser extension

This is the production browser-control surface for FP-013. It offers an explicit **Send link to Fetchpath** context-menu action. Automatic capture is separate and off until the person enables it in the popup; then the extension watches browser downloads, hands supported ones to the native host, and tries to restart a browser download if the native host refuses it. Unsupported URLs, denied site permissions, and excluded origins remain in the browser.

On the first send for an origin, the browser asks for that exact HTTP/HTTPS host permission. Applicable cookies are sent only through native messaging after the user gesture. The Windows host validates their domain/path/secure scope, protects the source URL, referrer, and cookie records with current-user DPAPI, commits a durable idempotent inbox record, and only then acknowledges. Routine extension events and inbox JSON contain a query-free URL and cookie count, never cookie names, values, or signed query values.

`manifest.chromium.json` has the fixed unpacked-extension key expected by the native host and covers Chrome and Edge API behavior. `manifest.firefox.json` carries Firefox's distinct add-on identity and background declaration. The installer materializes the native-host manifests beside `fetchpath-browser-host.exe` and registers them for each browser.

The [privacy disclosure](../../docs/user/BROWSER-PRIVACY.md) describes the
extension's local data use. A store package can be assembled from the eight
Chromium files shipped in the installer, with `manifest.chromium.json` renamed
to `manifest.json` at the ZIP root. A store upload can assign a different ID:
before a store install is offered, the published Chrome and Edge IDs must be
added to both the installed native-host manifest and the host's caller check,
then verified in each browser. The fixed unpacked ID remains for 0.1.0.

The manifest permissions match current features: `contextMenus` adds explicit
send and site-exclusion actions; `cookies` reads cookies for a permitted site;
`nativeMessaging` sends captures to the installed app; `storage` keeps local
settings and recent redacted results; `activeTab` lets the popup use the page
the person opened it on. `downloads` and HTTP/HTTPS host access are optional;
automatic capture requests them only when turned on, while explicit sending
requests access to the chosen site.

Run the policy and native tests from the repository root:

```powershell
node --test extensions/browser/test/*.test.mjs
cargo test -p fetchpath-desktop --locked
```

The real Chromium check uses the cached official Chrome for Testing build from FP-006, an isolated profile, a temporary current-user native-host registration that is restored in `finally`, and localhost-only fixtures:

```powershell
pwsh -NoProfile -File extensions/browser/test/run-chromium-runtime.ps1
```
