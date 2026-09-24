# Browser handoff spike (FP-006)

> Historical record. The source it describes (`tools/spikes/browser`) was retired on 23 September 2026 after production code superseded it; the commands below need it restored from history first: `git restore --source b7a5fc3 -- <path>`.

**Result: PASS for the documented capability matrix and an automated Chrome runtime proof; Edge and Firefox runtime execution remain NOT RUN.**

Run date: 20 September 2026 on Windows x64 with the official Chrome for Testing stable 153.0.8010.52. The machine also had Google Chrome 153.0.8010.50; Edge and Firefox executables were not detected in the standard install locations. Chrome for Testing was used because branded Chrome removed the `--load-extension` test switch in version 137. The runner uses a fresh ignored profile and never opens the user's normal browser profile.

## Compatibility matrix

`API` means the browser publishes the required extension/native-messaging surface. `PASS` means this repository executed the case against the local fixture. `NOT RUN` is not treated as runtime proof.

| Case | Chrome 153 runtime | Edge API/runtime | Firefox API/runtime | Fetchpath handoff decision |
| --- | --- | --- | --- | --- |
| Plain HTTP GET | PASS: native host durably acknowledged; browser download then reached `interrupted` | API: Chromium WebExtensions and Edge native messaging; NOT RUN | API: WebExtensions downloads and native messaging; NOT RUN | Eligible only after complete request observation and native acknowledgement |
| Authenticated GET | PASS for cookie auth: a scoped cookie was detected without copying its name/value; host rejected; browser continued. Other auth is UNSUPPORTED by this proof | API; NOT RUN | API; NOT RUN | Fallback until FP-013 can prove replayability and create an explicit origin-scoped `credential_ref` without putting raw credentials in the job command or log |
| POST-generated download | PASS: method observed as POST; host rejected; browser continued | API; NOT RUN | API includes request-body observation; NOT RUN | Fallback. Seeing some form/raw body fields is not a complete, durable replay contract |
| `blob:` URL | PASS: host rejected the non-HTTP scheme; browser continued | API; NOT RUN | API; NOT RUN | Fallback. `webRequest` does not export page-owned blob bytes as a native transfer source |
| Expiring/signed GET URL | PASS: host received the URL transiently, persisted only the query-free form, acknowledged, and the browser download reached `interrupted` | API; NOT RUN | API; NOT RUN | Eligible when otherwise unauthenticated. Query values remain secret and expiry/refresh must be surfaced by the production engine |

The Chromium manifest is shared by Chrome and Edge at the extension API level, but their Windows native-host registration keys and store packaging are separate. Firefox uses an explicit Gecko add-on ID and `allowed_extensions`, rather than Chromium's `allowed_origins`. The tracked manifests encode those differences instead of assuming runtime parity.

## Acknowledged handoff contract demonstrated

The fixture extension observes only `http://127.0.0.1/*` and uses supported `downloads`, `webRequest`, `cookies`, `storage`, and `nativeMessaging` APIs. It does not request a blocking web-request listener.

1. `downloads.onCreated` produces a candidate. HTTP candidates wait up to 250 ms for request-header observation; an incomplete observation is rejected rather than assumed safe.
2. The cookie API is queried for the exact URL under the manifest's host scope. Only a boolean `has_auth_context` crosses the native boundary; cookie names and values do not.
3. The native host checks the browser-supplied caller identity, a 1 MiB input limit, schema version, HTTP(S) GET method, URL-embedded credentials, complete observation, and absence of the authentication context visible to this spike.
4. For an accepted capture, the host appends a query-free ledger record, flushes it, calls `sync_all`, and only then returns `capture_ack`. The spike also sent the same capture ID twice: the first call created one record and the second returned `deduplicated: true` without another record.
5. The extension calls `downloads.cancel` only after `accepted: true` and records whether the item actually became `interrupted`. A rejection, native-host error, or incomplete observation leaves the browser path untouched.

This ordering prevents an unacknowledged handoff from discarding the browser download. It does not by itself make the production coordinator durable: FP-013 must bind the acknowledgement to the coordinator's idempotent command transaction from `JOB-CONTRACT.md`, not to this spike ledger.

## Security observations

- The native host manifest allowlists one fixed extension origin; the host independently checks the caller argument supplied by the browser.
- Chrome documents that `Authorization` and `Proxy-Authorization` are not provided to `onBeforeSendHeaders`, and that the API is an abstraction rather than the final wire request. Therefore, “no cookie/header observed” is not a general proof of an unauthenticated request. The passing plain/signed cases are controlled fixtures only. FP-013 must either use an explicit send-to-Fetchpath flow whose request context it constructs, establish a stronger supported capability signal, or preserve the browser path; this spike does not authorize generic automatic cancellation.
- The accepted ledger contains capture ID, method, query-free URL, and a signed-URL hint. The automated evidence asserts that signed query values, the POST fixture secret, and cookie material are absent.
- A signed URL necessarily crosses the local native-messaging pipe so the native engine can fetch it. It must be held as protected source/credential material and excluded from routine events, diagnostics, and telemetry.
- The extension records only redacted URLs. Cookie inspection is exact-URL and boolean-only in this spike. Production should request host access per site or explicit user action rather than defaulting to broad permanent access.
- The spike host's JSONL scan is sufficient for a serialized fixture, not a concurrent production ledger. The coordinator command ledger remains the authority for cross-process idempotency.

## Reproduce and evidence

From the repository root:

```powershell
powershell -ExecutionPolicy Bypass -File tools/spikes/browser/run-browser-spike.ps1
```

The first run downloads the official stable Chrome for Testing archive into ignored `work/browser-spike/`; later runs reuse it. The script builds and tests the Rust host, prepares the unpacked extension in the ignored work area, temporarily registers the exact HKCU Chrome native-host key, restores any previous key value in `finally`, runs the five fixture cases plus the duplicate probe, and writes [browser-spike.json](evidence/browser/browser-spike.json).

Observed run summary:

- Native messaging probe: PASS.
- Plain GET and signed GET: PASS; durable acknowledgement preceded an observed browser cancellation.
- Authenticated GET, POST, and blob: PASS; each stayed on the browser fallback path for the expected reason.
- Duplicate capture ID: PASS; one ledger record, second acknowledgement marked deduplicated.
- Secret persistence assertion: PASS.
- Native host unit test: PASS.

The loopback ports, download IDs, and blob UUID in the JSON are per-run observations. The Chrome archive is cached for reproducibility, but a locally calculated archive hash would only describe the received file and would not establish publisher authenticity.

## Browser API basis (verified 20 September 2026)

- Chrome documents native-host allowlisting, Windows registry discovery, length-prefixed JSON over stdio, and `sendNativeMessage` process behavior: [Chrome native messaging](https://developer.chrome.com/docs/extensions/develop/concepts/native-messaging).
- Chrome's downloads API can observe and cancel downloads: [Chrome downloads API](https://developer.chrome.com/docs/extensions/reference/api/downloads).
- Chrome's Manifest V3 `webRequest` remains available for observation and request-body metadata, while blocking access is restricted for ordinary extensions and the final wire headers are not fully exposed: [Chrome webRequest API](https://developer.chrome.com/docs/extensions/reference/api/webRequest).
- Microsoft documents the Edge extension-to-Win32 native messaging model: [Edge native messaging](https://learn.microsoft.com/en-us/microsoft-edge/extensions/developer-guide/native-messaging).
- Mozilla documents Firefox native messaging, its `allowed_extensions` difference, and its Windows registry location: [Firefox native messaging](https://developer.mozilla.org/en-US/docs/Mozilla/Add-ons/WebExtensions/Native_messaging).
- Mozilla documents request-body observation and the downloads API: [Firefox `webRequest.onBeforeRequest`](https://developer.mozilla.org/en-US/docs/Mozilla/Add-ons/WebExtensions/API/webRequest/onBeforeRequest), [Firefox downloads API](https://developer.mozilla.org/en-US/docs/Mozilla/Add-ons/WebExtensions/API/downloads).
- Google announced removal of `--load-extension` from branded Chrome 137 and directs automated extension testing to supported tooling/builds: [Chrome extensions update, June 2025](https://developer.chrome.com/blog/extension-news-june-2025).

## Limits and next gate

This proves the matrix decisions, acknowledgement mechanics, and real Chrome native IPC on controlled fixtures, not a production extension or universal browser compatibility. Edge and Firefox must still be run on supported installed versions. The spike cannot prove that an arbitrary Chromium download lacks hidden `Authorization` or proxy credentials, does not fetch accepted bytes in the native host, refresh expired URLs, create protected credential references, survive a browser/host/coordinator crash, package/register a signed executable, request user-approved site scopes, exercise redirects across authorization boundaries, or test store review/install/update behavior.

FP-013 should reuse the acknowledgement order and rejection reasons, connect acceptance to the durable coordinator command ledger, add explicit authenticated-capture consent and protected credential storage, run the matrix in Chrome, Edge, and Firefox, and test cancellation/completion races with both slow and very small responses.
