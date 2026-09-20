# Fetchpath

A Windows-first download manager for everyday and power users, with video/audio downloading included in the first public release. A portable Rust engine is the proposed production foundation.

**Current state:** M1 core work. The repository now has a working Rust file-job slice and CLI alongside the product plan, executable backlog, simulated desktop UX prototype, HTTP fixtures, and media/backend spikes. It is not a public-release download manager yet.

## Start here

- [Project direction and current phase](PROJECT.md)
- [Architecture and roadmap](docs/architecture/PLAN.md)
- [Repository map and boundaries](docs/architecture/REPOSITORY.md)
- [UX contract](docs/product/UX.md)
- [Codex workflow](docs/development/WORKFLOW.md)
- [Current tasks](docs/tasks/backlog.json)
- [Foundation validation](docs/development/FOUNDATION-RESULTS.md)
- [First native download](docs/development/FIRST-DOWNLOAD.md)

## Run and verify

Requires Node.js 24+ and Git. No npm installation or third-party JavaScript dependencies are needed.

```powershell
node tools/tasks.mjs next
node tools/tasks.mjs show FP-004
node tools/tasks.mjs check
node --test
node tools/serve-prototype.mjs
```

Open the loopback URL printed by the prototype server. It contains simulated jobs, not real transfers. See [benchmark instructions](tools/bench/README.md) for local fixture and curl baseline commands. Generated files belong under ignored `work/`; promote a small verified summary into the validation record.

The native CLI needs Rust 1.98+:

~~~powershell
$cargo = Join-Path $env:USERPROFILE '.cargo\bin\cargo.exe'
& $cargo test --workspace --locked
& $cargo build --workspace --locked
target\debug\fetchpath.exe download https://example.com/ example.html
~~~

Fetchpath currently accepts only a final HTTP 200 full representation. It refuses to overwrite a destination and reports an observed SHA-256 after safe publication. The desktop prototype still uses simulated jobs.

## Working with Codex

Open this folder as the Fetchpath Codex project and use Astra High for architecture/integration. Start a task with its backlog ID. [AGENTS.md](AGENTS.md) defines bounded retrieval, task ownership, evidence, and escalation. Smaller agents receive explicit file ownership and acceptance criteria.

The repository has no remote or published releases yet. Licensing/distribution and minimum supported Windows versions remain decisions before packaging.
