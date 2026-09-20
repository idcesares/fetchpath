# Windows HTTP backend spike

This tracked experiment packages a minimal Rust client with a statically built
libcurl and verifies its actual runtime capabilities. It is a decision tool,
not production downloader code.

From the repository root in PowerShell:

```powershell
$cargo = Join-Path $env:USERPROFILE '.cargo/bin/cargo.exe'
$env:CARGO_HOME = Join-Path (Get-Location) 'work/http-backend-cargo-home'
$env:CARGO_TARGET_DIR = Join-Path (Get-Location) 'work/http-backend-target'
& $cargo build --locked --manifest-path tools/spikes/http-backend/Cargo.toml
node tools/spikes/http-backend/run-spike.mjs
```

The build/cache, executable, and raw run output stay under ignored `work/`.
The normalized reviewed evidence lives in
`docs/development/evidence/http-backend/http-backend-spike.json`.

The runner uses loopback HTTP servers for H1, proxy, and cancellation checks.
It makes one public request to `https://example.com/` to exercise Windows TLS
trust with certificate and hostname verification enabled.
