# Task workflow

Open the Fetchpath folder in Codex or Claude Code and keep the strongest model as the lead (Astra High or Opus). Repository instructions express routing intent; no project file silently changes either tool's model settings. Both tools read `AGENTS.md`.

1. Run `node tools/tasks.mjs next`; it shows active work, then ready tasks ordered by priority (P0 blocks the phase, P1 is a headline outcome, P2 is next, P3 is later or research). Choose the highest priority ready task unless the user says otherwise.
2. Read `node tools/tasks.mjs show FP-XXX` and its relevant specification. Resolve blocking contracts before delegation. For a task larger than one sitting, write its implementation plan in `docs/development/plans` first.
3. Lead records owner/status, a branch such as `fp-004-http-backend-spike`, file ownership, and exact acceptance. Use a worktree after the first baseline commit exists.
4. Delegate independent tasks to a suitable smaller model. Start with two implementers at most. Lead retains shared-file and integration ownership.
5. Implement the smallest working behavior. Record commands, results, and limitations. Strong-model review covers protected invariants and is mandatory for tasks marked `review: "strong"`.
6. Integrate, run affected checks, update evidence and task state, then choose the next ready task. Do not dispatch blocked dependants.

The executable backlog replaces scattered TODO lists. Milestones remain in the plan; a task can be complete while its milestone still has outstanding tasks. `next` reports eligible work, not an instruction to execute every task automatically.

## Handoff packet

Use [the task template](../tasks/TEMPLATE.md). Give the agent only the relevant contract and files. Preserve explicit command outputs when a conclusion depends on them; summarize routine successful checks. After two failed repair attempts, send evidence back to the lead instead of extending the retry loop indefinitely.

The initial model pairing is a practical trial. Measure accepted changes, corrections and wall time; compare cost only when actual usage is available. Never claim efficiency solely because the agent used a smaller model.

## Daily verification

`node tools/tasks.mjs check` validates IDs, statuses, ownership, dependencies, cycles, done prerequisites, and evidence paths. `node --test` runs the repository, fixture, extension and installer tests, including `tests/repo/structure.test.mjs`, which keeps the [repository map](../architecture/REPOSITORY.md), documentation links and workspace entries in step with the tree. The Rust and desktop checks are listed in [CONTRIBUTING.md](../../CONTRIBUTING.md); CI (`.github/workflows/checks.yml`) runs only the Node checks.

## Keeping the tree clean

- A new component is added to the repository map in the same change; the structure test enforces it.
- Task records go in `docs/development` and are listed in [its index](README.md); design specs in `docs/architecture/specs`; implementation plans in `docs/development/plans`. Tool-specific default locations are not used.
- Spikes and prototypes are disposable. Once production code supersedes one, delete it, keep its record and evidence, and point the record at the commit that still holds the source.
- Scratch output lives in ignored `work/` and must be safe to delete; anything a record depends on is promoted into `docs/development/evidence`.

GitHub CI cannot be described as passed until a remote run occurs. Publishing a remote repository remains separate from organizing this local project.
