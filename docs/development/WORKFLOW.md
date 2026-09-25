# Task workflow

The lead is the strongest model the user selected (Astra High in Codex, Opus in
Claude Code). Both tools read `AGENTS.md`.

1. `node tools/tasks.mjs next` lists active work, then ready tasks by priority
   (P0 blocks the phase, P1 is a headline outcome, P2 next, P3 later or
   research). Take the highest unless the user says otherwise.
2. `node tools/tasks.mjs show FP-XXX`; read only the contracts and source it
   names. Record status `in_progress`, owner and file area in the backlog.
3. Build the smallest working behavior with the tests that guard its contract.
4. Tasks marked `review: "strong"` get an independent lead-tier review that
   derives failure cases from the contract before the task is done. Other
   tasks get the lead's own check; no review ceremony for routine edits.
5. Run the affected checks, update the task's record and evidence, mark it
   done, commit, and take the next task.

After two failed repair attempts, return the evidence to the lead instead of
retrying.

## Model tiers

The backlog's `modelTier` names a role. Record the model that actually did the
work.

| `modelTier` | Role | Codex | Claude Code |
|---|---|---|---|
| `astra-high` | Lead: contracts, persistence, security, integration | Astra High | Opus |
| `terra-medium` | Bounded implementation against frozen interfaces | Terra Medium | Sonnet |
| (mechanical) | Docs, fixtures, formatting | smaller fast model | Haiku |

Choose by cost per accepted change, not tokens; make no savings claim without
measured usage. No model call belongs in the download data path.

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
