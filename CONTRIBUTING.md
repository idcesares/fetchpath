# Working on Fetchpath

[AGENTS.md](AGENTS.md) defines bounded retrieval, task ownership, evidence and
escalation. Start a task with its backlog ID and record `in_progress`, an owner
and a narrow owned file area before changing anything.
[PROJECT.md](PROJECT.md) records the current phase and
[docs/tasks/backlog.json](docs/tasks/backlog.json) is authoritative for task
state; nothing else in the repository duplicates it.

The engine is a portable Rust core; the desktop client is Tauri 2 on
Microsoft-serviced Windows 11 x64.

## Run and verify

Node.js 24+ and Git are enough for the repository checks. No npm install and no
third-party JavaScript dependencies are needed for them.

```powershell
node tools/tasks.mjs next          # the next ready task
node tools/tasks.mjs show FP-018   # one task in full
node tools/tasks.mjs check         # backlog graph, contracts and evidence
node --test                        # fixture, extension and backlog tests
node tools/licenses/generate.mjs --check   # third-party notices are current
node tools/media-tools/pin.mjs --check     # media helper pins still match their publishers
```

The native-host test in `node --test` runs `target/debug/fetchpath-browser-host.exe`,
so after a `cargo clean` run `cargo build --workspace` first.

The Rust workspace needs Rust 1.98+:

```powershell
$cargo = Join-Path $env:USERPROFILE '.cargo\bin\cargo.exe'
& $cargo test --workspace --locked
& $cargo clippy --workspace --all-targets -- -D warnings
& $cargo fmt --check
& $cargo build --workspace --locked
target\debug\fetchpath.exe download https://example.com/ example.html
```

The real guided media-tool install downloads about 130 MB from the helpers'
publishers, so it is ignored by default:

```powershell
& $cargo test -p fetchpath-desktop --locked -- --ignored guided_install
```

The desktop application needs `pnpm` through corepack:

```powershell
corepack pnpm --dir apps/desktop install
& $cargo build -p fetchpath                    # the engine the app starts, beside it
corepack pnpm --dir apps/desktop tauri dev     # run it
corepack pnpm --dir apps/desktop release       # optimized build and installer, with the CLI
```

`release` stages the `fetchpath` command line as a sidecar and builds the NSIS
installer at `target/release/bundle/nsis/`. Plain `tauri build` still works but
leaves the CLI out.

On-machine compatibility harnesses, which install and drive the real artifacts:

```powershell
pwsh -NoProfile -File tests/compatibility/windows/ui-accessibility.ps1
pwsh -NoProfile -File tests/compatibility/windows/packaging-lifecycle.ps1
pwsh -NoProfile -File tests/compatibility/metalink/run.ps1
```

Both Windows scripts throw on a failed assertion and still write their partial
observation, so a JSON file with `"passed": false` is a failure record rather
than a missing run. They drive the real window and inject keystrokes, so do not
use the machine while they run. See [benchmark instructions](tools/bench/README.md)
for fixture and curl baseline commands. Generated files belong under ignored
`work/`; promote a small verified summary into the validation record.

## Releasing

- Media helpers are pinned in `adapters/media/media-tools.json` with
  `node tools/media-tools/pin.mjs`, which records a digest only when the
  download matches the publisher's own checksum file. Pin only permanent,
  versioned release URLs.
- Interim builds are unsigned by decision. The Authenticode hook in the bundle
  is inert with no certificate configured; setting `FETCHPATH_SIGN_THUMBPRINT`
  signs every binary in the bundle with no other change.
- Publish the installer's SHA-256 next to the download; the user guide tells
  people to check it.
- [RELEASE-CANDIDATE.md](docs/development/RELEASE-CANDIDATE.md) holds the
  acceptance matrix and the release decision.

Local development does not imply publishing, paid provisioning, external
messages, or destructive actions.

## Documentation

- [Project direction and current phase](PROJECT.md)
- [Architecture and roadmap](docs/architecture/PLAN.md)
- [Repository map and boundaries](docs/architecture/REPOSITORY.md)
- [UX contract](docs/product/UX.md)
- [Development workflow](docs/development/WORKFLOW.md)
- [Release candidate record](docs/development/RELEASE-CANDIDATE.md)
- [Windows UX and packaging evidence](docs/development/WINDOWS-PACKAGING.md)
- [Metalink repair evidence](docs/development/METALINK-REPAIR.md)
- [Content cache and paired LAN evidence](docs/development/CACHE-AND-LAN.md)
- [Media integration evidence](docs/development/MEDIA-INTEGRATION.md)
- [Browser capture evidence](docs/development/BROWSER-CAPTURE.md)
