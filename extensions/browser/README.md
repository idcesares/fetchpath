# Fetchpath browser extension

This is the production browser-control surface for FP-013. It offers an explicit **Send link to Fetchpath** context-menu action. Automatic capture is separate and off until the person enables it in the popup; then the extension watches browser downloads, hands supported ones to the native host, and tries to restart a browser download if the native host refuses it. Unsupported URLs, denied site permissions, and excluded origins remain in the browser.

Manual sends (**Send link**, **Send page**, **Send video**, and the popup button) send immediately without requesting per-site host permission (`cookies: []`). If the person has opted in to share sign-in cookies for that site via the popup toggle (or enabled automatic capture), applicable cookies are filtered to RFC 6265 scope and sent only through native messaging. The Windows host independently validates their domain/path/secure scope, protects the source URL, referrer, and cookie records with current-user DPAPI, commits a durable idempotent inbox record, and only then acknowledges. Routine extension events and inbox JSON contain a query-free, credential-free URL and cookie count, never cookie names, values, or signed query values.

`manifest.chromium.json` has the fixed unpacked-extension key expected by the native host and covers Chrome and Edge API behavior. `manifest.firefox.json` carries Firefox's distinct add-on identity and background declaration. The installer materializes the native-host manifests beside `fetchpath-browser-host.exe` and registers them for each browser.

The [privacy disclosure](../../docs/user/BROWSER-PRIVACY.md) describes the
extension's local data use. A store package can be assembled from the nine
Chromium files shipped in the installer, with `manifest.chromium.json` renamed
to `manifest.json` at the ZIP root. A store upload can assign a different ID:
before a store install is offered, the published Chrome and Edge IDs must be
added to both the installed native-host manifest and the host's caller check,
then verified in each browser. The fixed unpacked ID remains for unpacked
installs.

The manifest permissions match current features: `contextMenus` adds explicit
send and site-exclusion actions; `cookies` reads cookies for a permitted site;
`nativeMessaging` sends captures to the installed app; `storage` keeps local
settings and recent redacted results; `activeTab` lets the popup use the page
the person opened it on. `downloads` and HTTP/HTTPS host access are optional;
automatic capture requests them only when turned on (and revokes them when
turned off), and per-site cookie sharing requests exact-origin access only when
toggled on in the popup.

Run the policy and native tests from the repository root:

```powershell
node --test extensions/browser/test/*.test.mjs
cargo test -p fetchpath-desktop --locked
```

The real Chromium check uses the cached official Chrome for Testing build from FP-006, an isolated profile, a temporary current-user native-host registration that is restored in `finally`, and localhost-only fixtures:

```powershell
pwsh -NoProfile -File extensions/browser/test/run-chromium-runtime.ps1
```
