# Task workflow

Preserve the model the user explicitly selected. Otherwise Codex starts with
GPT-6.1 Sol/low, using medium for substantive implementation and Astra/low for
difficult judgment or consequential review. Claude Code keeps Opus for lead work.
Both tools read `AGENTS.md`.

1. `node tools/tasks.mjs next` lists active work, then ready tasks by priority
   (P0 blocks the phase, P1 is a headline outcome, P2 next, P3 later or
   research). Take the highest unless the user says otherwise.
2. `node tools/tasks.mjs show FP-XXX`; read only the contracts and source it
   names. Record status `in_progress`, owner and file area in the backlog.
3. Build the smallest working behavior with the tests that guard its contract.
4. Tasks marked `review: "strong"` get an independent boundary review before
   done. Use Astra/low for the first integrity, persistence, unsafe/FFI or
   credential assessment. After that, named repairs normally get one independent
   Sol/medium recheck (Sonnet in Claude Code), escalating only a concrete
   unresolved boundary question. Other tasks get the lead's own check.
5. Run the affected checks, update the task's record and evidence, mark it
   done, commit, and take the next task.

After two failed repair attempts, return the evidence to the lead instead of
retrying.

Write review packets with prior findings, exact changed symbols, affected
invariants and existing passing evidence. Review only that scope and necessary
dependencies. Return one concise verdict and concrete blockers; reopen only
affected findings after a repair. Do not restart an architecture audit or
repeat passed checks without changed code or a specific unresolved concern.

Each performance check must name the acceptance claim and decision it supports.
Reuse recorded paired runs; run only a missing targeted comparison, then read
its compact summary. Remove or explicitly defer unproven optimizations rather
than investigate indefinitely. Record limitations and adjust the relevant
contract openly if a deferral changes acceptance; never silently waive a gate.

## Model tiers

The backlog's `modelTier` names a role. Record the model that actually did the
work. Legacy backlog tier labels identify roles; they do not force an obsolete model.

| `modelTier` | Role | Codex | Claude Code |
|---|---|---|---|
| `astra-high` | Lead: contracts, persistence, security, integration | GPT-6 Astra/low; high when justified or selected | Opus |
| `terra-medium` | Bounded implementation against frozen interfaces | GPT-6.1 Sol/medium | Sonnet |
| (repair recheck) | Independent recheck after the initial boundary review | GPT-6.1 Sol/medium | Sonnet |
| (mechanical) | Docs, fixtures, formatting | GPT-6 Luna/high | Haiku |

Choose by cost per accepted change, not tokens; make no savings claim without
measured usage. No model call belongs in the download data path.

The project `.codex/config.toml` sets Sol/low for ordinary launches and Sol/medium
for default children and the `bounded_reviewer` role. Explicit user selections
take precedence. These launch defaults do not change the active root model.
Setting names follow the [official configuration reference](https://learn.chatgpt.com/docs/config-file/config-reference)
(checked 30 September 2026).

## Keeping the tree small

Every file an agent may read costs tokens in every later session, so:

- **Plans are scratch.** Write an implementation plan, if one is needed, under
  ignored `work/plans/`; it is never committed and is deleted with the task.
- **One short record per task area.** A task record in `docs/development`
  states what was built, how it was verified, and the limitations that still
  hold. Update it in place; do not append logs, review transcripts or
  follow-up diaries. Review findings go in commit messages.
- **Evidence only when a claim depends on it.** Raw evidence goes in
  `docs/development/evidence/<area>/` and stays small; output that can be
  regenerated stays in `work/`.
- **Tests guard contracts.** Add a test for behavior a person or client relies
  on, a persisted format, or a protected invariant (A01–A13). Do not add
  tests that restate implementation details or documentation.
- **Retire what is superseded.** When code or a record is superseded, delete
  it and list it in [the archive](ARCHIVE.md) with the commit that holds it.
- A new component enters the [repository map](../architecture/REPOSITORY.md)
  in the same change; `tests/repo/structure.test.mjs` enforces it.

## Checks

`node tools/tasks.mjs check` validates the backlog and its evidence paths.
`node --test` runs the repository, fixture, extension and installer tests.
Rust and desktop checks are in [CONTRIBUTING.md](../../CONTRIBUTING.md); CI
runs only the Node checks. CI cannot be called passed until a remote run
occurs.
