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
- Superseded by FP-056 (below): the host hands each capture to the engine, starting one if needed, and still opens the window.
- Chrome runtime behavior is proven. The Chromium manifest is shared with Edge at the API level, and a distinct Firefox manifest is present, but Edge and Firefox runtime/store testing remain release compatibility gates.
- Native-host manifest installation and browser-specific registry registration are installer responsibilities. The current manifests are templates and the executable is produced by the desktop Rust package; no unsigned development artifact is presented as distributable.
- *Update, 23 September 2026 (FP-036):* the installer now installs host manifests with a relative `path` beside the host, registers them under HKCU for Chrome, Edge and Firefox, ships the unpacked Chromium extension, and removes the three keys on uninstall. Settings reports the registration and guides Load unpacked. See [release readiness](ARCHIVE.md).
- DPAPI protects data for the current Windows user on the current machine. Secret plaintext necessarily exists briefly in extension/native/core process memory while authorized work is prepared. Power-loss durability and hostile same-user process isolation are not claimed.

## FP-056: captures reach the engine without the window

Recorded 25 September 2026 on Windows 11 Pro 26200 x64. Code:
`apps/desktop/src-tauri/src/browser_bridge.rs` (`nudge_engine`),
`crates/fetchpath-protocol/src/launch.rs` (`nudge_for_browser`), the
`TakeBrowserCaptures` command and `CapturesTaken` result, and the session's
intake and `take_link_reviews`. Tests:
`apps/desktop/src-tauri/tests/browser_host.rs`, the session's
`a_captured_media_page_goes_to_review_instead_of_the_file_queue`, and the
browser principal in `crates/fetchpath-session/tests/policy.rs`.

| Part | What it does |
|---|---|
| Handoff | The DPAPI inbox stays the durable handoff. After a capture is accepted (or found to be a duplicate), the host connects as the `browser` principal and sends `TakeBrowserCaptures`, which takes the inbox in at once. With no engine, it starts the `fetchpath.exe` beside it, which takes the inbox in before it serves. All of this happens within 3 seconds, before the browser gets its answer; the window is still opened as before |
| Principal | `browser` may send `CreateJob` and `TakeBrowserCaptures`, nothing else; agents may not send the latter |
| Nothing lost | Intake is idempotent by capture id and credential reference. With no engine to start (none installed beside the host, or setup's update hold), the capture waits in the inbox and the next engine takes it in |
| Media pages | A page on a media site is not a file job; it waits for a window's **Add download**. Its capture now stays pending in the inbox until a client takes it with `TakeLinkReviews`, and only then is marked done and its unused cookie envelope deleted. Before, it was marked done at intake and held in memory, so an engine that left before a window opened lost it, and its envelope was never deleted |

### Checks

- `browser_host.rs`, the real host and engine as processes from a folder
  with no desktop app, three passes in a row. With nothing running, a capture
  started the engine and reached the queue within the host's answer, with
  its signed query redacted; the same capture again was answered as a
  duplicate with no second job, and a second capture went to the running
  engine. A host with no engine beside it, and one under the update hold
  (answered in under 2 s, nothing started), left their captures pending until
  the next engine took both in. A media page survived an engine stop, was
  handed out exactly once, and left the inbox empty.
- Session and policy tests as above; `cargo test -p fetchpath-protocol`
  with the schema regenerated (one command, one result).

| Temporary change | Failing test |
|---|---|
| The host does not nudge | `a_capture_reaches_the_queue_…`, `a_media_page_waits_…` |
| Media pages marked done at intake (the old way) | `a_media_page_waits_…`, `a_captured_media_page_goes_to_review_…` |

### Limitations

- The host waits up to 3 s for an engine to answer. After that the capture
  still reaches the queue when the engine it started comes up, but a browser
  that ends the host at once could also end an engine it failed to move out
  of its job object.
- Engines from before FP-056 answer `contract.unknown_command`; the host
  takes that as a nudge, and they take the inbox in at their next tick.
- No real-browser run was repeated for this change; the framed host protocol
  the tests drive is the one Chrome uses.
