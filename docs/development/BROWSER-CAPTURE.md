# Production browser capture evidence

Verified 21 September 2026 for FP-013 and acceptance criteria A07/A08.

## Delivered boundary

Fetchpath exposes an explicit **Send link to Fetchpath** browser context-menu action. The extension does not observe, cancel, or replace ordinary browser downloads, so an unsupported URL, denied site permission, excluded origin, native-host failure, POST, or `blob:` workflow remains on the browser path.

The first send for a site requests that exact HTTP/HTTPS origin. After the user gesture, the extension reads only cookies applicable to the selected URL and sends a versioned GET capture over native messaging. It stores only bounded redacted events: capture ID, query-free URL, result, and cookie count. Site exclusions are exact-origin entries in extension-local storage and are checked before permission or native-host work.

The Windows native host independently enforces its fixed Chrome/Edge extension origin or Firefox add-on ID, a 1 MiB frame limit, schema version, UUID capture identity, explicit-action flag, HTTP(S) GET, no URL-embedded credentials, a safe leaf filename, same-origin redacted referrer, and cookie domain/path/secure scope. Cookie values become libcurl cookie-engine records, so they follow cookie scope rather than a redirect-forwarded raw `Cookie` header.

Signed query values, the origin-only referrer, and cookies are serialized into a current-user DPAPI envelope with application entropy. The public durable inbox contains only an opaque `credential_ref`, request fingerprint, safe filename, and query-free URL. Acknowledgement occurs only after the protected envelope and create-only inbox command have been flushed and synced. Reusing the same capture ID and fingerprint returns the existing result; a different fingerprint is an idempotency conflict.

The desktop polls this inbox with its normal queue refresh. It commits the queue record before marking the inbox processed, and detects an already-ingested credential reference after a crash between those steps. Browser jobs choose a non-conflicting filename in the user's Downloads directory. The protected context is retained for restart/retry, omitted from queue JSON and UI state, and removed after completion or history removal.

## Verification

The real-browser runner loaded the production Chromium code into official Chrome for Testing 153.0.8010.52 with an isolated profile and localhost-only fixture. Its temporary native-host registry value was restored in `finally`. The run observed:

- one explicit capture durably accepted over `chrome.runtime.sendNativeMessage`;
- one HttpOnly cookie available only inside the protected request context;
- one public inbox record containing no signed query, cookie name, or cookie value;
- an exact-origin exclusion that returned `site_excluded` and created no second command.

The retained machine-readable observation is [browser-capture.json](evidence/browser/browser-capture.json). Reproduce it with:

```powershell
pwsh -NoProfile -File extensions/browser/test/run-chromium-runtime.ps1
```

Additional automated coverage includes:

- Node policy/background tests for HTTP-only sources, exact-origin exclusions, filename derivation, redacted extension storage, cookie mapping, and native fallback.
- A framed native-host process test for caller identity, DPAPI ciphertext, durable acknowledgement, deduplication, and unsupported POST rejection.
- Rust bridge tests for malformed IPC limits, URL credentials, reserved/path-escape filenames, cookie scope mismatch, protected-secret recovery, and idempotency conflict.
- Core transfer tests proving an authorized cookie reaches its matching host and does not cross a redirect from `127.0.0.1` to `localhost`.
- Desktop integration proving one protected inbox command becomes one persisted queue job and one correct downloaded file, without the signed query entering queue JSON.

## Current limits

- Capture is deliberately explicit. Generic automatic cancellation remains unsafe because browser observation cannot prove that a Chromium request lacks hidden `Authorization` or proxy credentials.
- Version 1 supports replayable GET links, including signed URLs and browser cookies. POST bodies, page-owned blobs, client certificates, proxy credentials, and hidden authorization headers remain in the browser.
- The native host writes a durable inbox but does not launch Fetchpath. An open desktop consumes it on the next queue poll; otherwise it is consumed on the next launch.
- Chrome runtime behavior is proven. The Chromium manifest is shared with Edge at the API level, and a distinct Firefox manifest is present, but Edge and Firefox runtime/store testing remain release compatibility gates.
- Native-host manifest installation and browser-specific registry registration are installer responsibilities. The current manifests are templates and the executable is produced by the desktop Rust package; no unsigned development artifact is presented as distributable.
- DPAPI protects data for the current Windows user on the current machine. Secret plaintext necessarily exists briefly in extension/native/core process memory while authorized work is prepared. Power-loss durability and hostile same-user process isolation are not claimed.
