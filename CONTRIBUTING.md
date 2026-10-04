# Working on Fetchpath

[AGENTS.md](AGENTS.md) defines bounded retrieval, task ownership, evidence and
escalation. Start a task with its backlog ID and record `in_progress`, an owner
and a narrow owned file area before changing anything.
[PROJECT.md](PROJECT.md) records the current phase and
[docs/tasks/backlog.json](docs/tasks/backlog.json) is authoritative for task
state; nothing else in the repository duplicates it.

The download core is portable Rust. One per-user engine (`fetchpath engine`,
in `apps/cli`) hosts the session (`crates/fetchpath-session`) and is the only
owner of the queue; the desktop (Tauri 2 on Microsoft-serviced Windows 11
x64), the command line, the interactive terminal, the browser host and the MCP
server are clients over protocol v1 (`crates/fetchpath-protocol`). The design
is [the engine platform spec](docs/architecture/specs/2026-09-24-engine-platform-design.md).

## Run and verify

Node.js 24+ and Git are enough for the repository checks. No npm install and no
third-party JavaScript dependencies are needed for them.

```powershell
node tools/tasks.mjs next          # the next ready task
node tools/tasks.mjs show FP-018   # one task in full
node tools/tasks.mjs check         # backlog graph, contracts and evidence
node tools/tests.mjs               # repository Node tests; excludes ignored work
node tools/licenses/generate.mjs --check   # third-party notices are current
node tools/media-tools/pin.mjs --check     # media helper pins still match their publishers
```

The runner uses an explicit repository test list so ignored worktrees and copied
plugin caches are not discovered as tests. `npm test` uses the same runner.
The native-host test runs `target/debug/fetchpath-browser-host.exe`,
so after a `cargo clean` run `cargo build --workspace` first.

The Rust workspace needs Rust 1.98+; CI validates with Rust 1.98.1:

```powershell
$cargo = Join-Path $env:USERPROFILE '.cargo\bin\cargo.exe'
& $cargo test --workspace --locked
& $cargo clippy --workspace --all-targets -- -D warnings
& $cargo fmt --check
& $cargo build --workspace --locked
target\debug\fetchpath.exe download https://example.com/ example.html
```

### Running the engine in development

The engine keeps its data in `%APPDATA%\app.fetchpath.desktop`, the same
folder an installed Fetchpath uses. Point `FETCHPATH_APP_DATA_DIR` at a
scratch folder to run an engine of your own beside it: the instance lock,
pipe secret and endpoint live in that folder, so the two never meet. Every
client started from that shell reaches the scratch engine. The content
cache and paired-device state use `FETCHPATH_DATA_DIR` instead
(default `%LOCALAPPDATA%\Fetchpath`); set it too for a fully separate run.

```powershell
$env:FETCHPATH_APP_DATA_DIR = "$PWD\work\engine-data"
$env:FETCHPATH_DATA_DIR = "$PWD\work\engine-cache"
target\debug\fetchpath.exe engine status   # starts nothing; says whether one runs
target\debug\fetchpath.exe add https://example.com/ --wait
target\debug\fetchpath.exe                 # the interactive terminal
target\debug\fetchpath.exe engine stop
```

`fetchpath mcp` speaks MCP on standard input and output, so drive it from an
agent host (see [CLI.md](docs/user/CLI.md#ai-agents-mcp)) or from the tests in
`apps/cli/tests`. The protocol's JSON Schema is checked in; after a deliberate
wire change, regenerate it with `FETCHPATH_BLESS_SCHEMA=1` set while running
`cargo test -p fetchpath-protocol` and review the diff.

CI also runs the ignored helper-policy and durable-publication regression in an
isolated `CARGO_TARGET_DIR`, keeping its deterministic helper away from real
artifacts. Repository test files run sequentially to protect fixture timing
from concurrent native installer checks.

The real guided media-tool install downloads about 130 MB from the helpers'
publishers, so it is ignored by default:

```powershell
& $cargo test -p fetchpath-media --locked -- --ignored guided_install
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

### Windows lifecycle checks

Prefer the disposable Sandbox harnesses for the current release. They install
only inside the guest and keep the owner's Windows session untouched. Run them
one at a time after building the release:

```powershell
powershell.exe -NoProfile -File tests/compatibility/windows/sandbox-lifecycle.ps1
powershell.exe -NoProfile -File tests/compatibility/windows/agent-installer.ps1
powershell.exe -NoProfile -File tests/compatibility/windows/sign-in-lifecycle.ps1
powershell.exe -NoProfile -File tests/compatibility/windows/sandbox-lifecycle.ps1 -BaselineInstallerPath work/baseline/Fetchpath_0.1.0_x64-setup.exe -Components cli
```

Download the published baseline and verify its release checksum before the
upgrade test. The sign-in harness performs an actual guest logoff/logon,
checks automatic engine startup before a client can start it, and verifies a
checkpointed download. It does not reboot the owner PC or test sudden power loss.
Sandbox adjusts Smart App Control only inside the disposable guest because the
installer is unsigned; ordinary users must follow the documented Windows limits.

The older on-machine `packaging-lifecycle.ps1` and `engine-lifecycle.ps1` are
historical 0.1.x harnesses with fixed version expectations. Do not use their
old artifact defaults as a current release gate. They require separately built
versioned fixtures and refuse artifacts older than the last commit.

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
- [Engine session](docs/development/ENGINE-SESSION.md), [protocol](docs/development/ENGINE-PROTOCOL.md) and [host](docs/development/ENGINE-HOST.md)
- [Interactive terminal](docs/development/TERMINAL.md), [smart rules](docs/development/RULES.md) and [agents through MCP](docs/development/MCP.md)
- [All development records](docs/development/README.md)
