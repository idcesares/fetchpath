# HTTP backend packaging spike (FP-004)

Checked on native Windows on 20 September 2026. This is a disposable, reproducible Rust/libcurl candidate under `tools/spikes/http-backend`; it is not a production downloader, installer, or performance result.

## Result

The candidate builds with Rust 1.98.1/Cargo 1.98.1 using an isolated `CARGO_HOME`, target directory, and tracked lockfile. It uses the `curl` crate 0.4.50 with `static-curl`, resolving `curl-sys` 0.4.90+curl-8.21.0. The resulting 1,256,960-byte debug executable was built and run locally; its SHA-256 is `df06a29a0bfc07ab5471326fd65ffa6540496131a834e6dcf91dec0a60d58744`. No `*curl*.dll` was present beside the executable. This is packaging evidence for the experiment, not proof of release dependency closure.

The runtime query reported libcurl `8.21.0-DEV` with Schannel, `http2: false`, and `http3: false`. libcurl documents that the runtime version structure exposes feature flags and that the HTTP2/HTTP3 flags denote whether those capabilities were built in. [curl_version_info](https://curl.se/libcurl/c/curl_version_info.html) Therefore this candidate is a viable H1/TLS/proxy/cancellation experiment, but it **fails the H2/H3 capability gate**. It must not be described as supporting either protocol.

| Check | Status | Evidence |
|---|---|---|
| Native Rust/libcurl build | Pass | `cargo build --locked` completed; isolated lockfile hash is `1ac9059ef20eb95ee45b1c9a3c48113f97dedb7d55969070c33636850cb9f91f`. |
| H1 local fixture | Pass | 1 MiB HTTP/1.1 fixture returned 200 and delivered 1,048,576 bytes. |
| Packaged H2 feature | Fail | Runtime `http2: false`; no H2 transfer was attempted because no compatible controlled endpoint was available. |
| Packaged H3 feature | Fail | Runtime `http3: false`; no H3 transfer was attempted because no compatible controlled endpoint was available. HTTP/3 needs a deliberately compatible libcurl/QUIC/TLS build. [curl HTTP/3 documentation](https://curl.se/docs/http3.html) |
| Public TLS trust | Pass | With peer/host verification enabled and proxy use explicitly disabled, `https://example.com/` returned 200 and 559 bytes. This proves one live public trust path at test time, not universal trust-store coverage. |
| Explicit HTTP proxy | Pass | A controlled local proxy observed the absolute-form GET and returned 502. The client surfaced response 502. The proxy option overrides environment proxy variables; an empty proxy explicitly disables them. [CURLOPT_PROXY](https://curl.se/libcurl/c/CURLOPT_PROXY.html) |
| Bounded cancellation | Pass | A slow 512 KiB local response was aborted at 65,536 received bytes with libcurl error 42 (`CURLE_ABORTED_BY_CALLBACK`). A nonzero progress callback aborts a transfer with that result. [CURLOPT_XFERINFOFUNCTION](https://curl.se/libcurl/c/CURLOPT_XFERINFOFUNCTION.html) |

The machine-readable normalized result is [http-backend-spike.json](evidence/http-backend/http-backend-spike.json). The tracked runner retains raw command/stdout/stderr output in `work/http-backend-evidence/http-backend-spike.json` after each reproduction. Both records deliberately mark H2/H3 transfers as `not-run` instead of manufacturing protocol results.

## Reproduce

From the repository root in PowerShell:

```powershell
$cargo = Join-Path $env:USERPROFILE '.cargo/bin/cargo.exe'
$env:CARGO_HOME = Join-Path (Get-Location) 'work/http-backend-cargo-home'
$env:CARGO_TARGET_DIR = Join-Path (Get-Location) 'work/http-backend-target'
& $cargo build --locked --manifest-path tools/spikes/http-backend/Cargo.toml
node tools/spikes/http-backend/run-spike.mjs
```

The first build downloads crate sources into `work/http-backend-cargo-home`; it does not install a global package or alter the repository root. `run-spike.mjs` starts only loopback fixture/proxy/slow-response servers, except for the single `https://example.com/` TLS check with libcurl proxy use disabled. The script always closes its local servers.

## Next decision

Keep Rust/libcurl as the H1 candidate only if the product can defer H2/H3. To pursue the planned opportunistic H2/H3 path, create a separate controlled packaging spike for a pinned libcurl build with its required HTTP/2 and QUIC/HTTP/3 dependencies, then repeat this exact capability, TLS, proxy, cancellation, and controlled-protocol matrix. Do not use this result to claim a speed benefit or protocol fallback behavior.
