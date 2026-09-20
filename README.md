# Fetchpath

A Windows-first download manager for everyday and power users, with video/audio downloading included in the first public release. A portable Rust engine is the proposed production foundation.

**Current state:** M0 foundation work. This repository contains the product plan, an executable task backlog, a simulated desktop UX prototype, and a local HTTP benchmark/fixture harness. It is not a working production downloader.

## Start here

- [Project direction and current phase](PROJECT.md)
- [Architecture and roadmap](docs/architecture/PLAN.md)
- [Repository map and boundaries](docs/architecture/REPOSITORY.md)
- [UX contract](docs/product/UX.md)
- [Codex workflow](docs/development/WORKFLOW.md)
- [Current tasks](docs/tasks/backlog.json)
- [Foundation validation](docs/development/FOUNDATION-RESULTS.md)

## Run the foundation

Requires Node.js 24+ and Git. No npm installation or third-party JavaScript dependencies are needed.

```powershell
node tools/tasks.mjs next
node tools/tasks.mjs show FP-004
node tools/tasks.mjs check
node --test
node tools/serve-prototype.mjs
```

Open the loopback URL printed by the prototype server. It contains simulated jobs, not real transfers. See [benchmark instructions](tools/bench/README.md) for local fixture and curl baseline commands. Generated files belong under ignored `work/`; promote a small verified summary into the validation record.

Rust is needed when the first engine slice starts. The initial environment has a working toolchain at `%USERPROFILE%\.cargo\bin`, although that directory was absent from this session's PATH. No production Rust crate or desktop framework has been committed to yet.

## Working with Codex

Open this folder as the Fetchpath Codex project and use Astra High for architecture/integration. Start a task with its backlog ID. [AGENTS.md](AGENTS.md) defines bounded retrieval, task ownership, evidence, and escalation. Smaller agents receive explicit file ownership and acceptance criteria.

The repository has no remote or published releases yet. Licensing/distribution and minimum supported Windows versions remain decisions before packaging.
