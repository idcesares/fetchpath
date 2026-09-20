# Codex task workflow

Open the Fetchpath folder in Codex and keep Astra High as the lead. Repository instructions express routing intent; no project file silently changes Codex model settings.

1. Run `node tools/tasks.mjs next`; choose a ready task.
2. Read `node tools/tasks.mjs show FP-XXX` and its relevant specification. Resolve blocking contracts before delegation.
3. Lead records owner/status, a branch such as `fp-004-http-backend-spike`, file ownership, and exact acceptance. Use a worktree after the first baseline commit exists.
4. Delegate independent tasks to a suitable smaller model. Start with two implementers at most. Lead retains shared-file and integration ownership.
5. Implement the smallest working behavior. Record commands, results, and limitations. Strong-model review covers protected invariants.
6. Integrate, run affected checks, update evidence and task state, then choose the next ready task. Do not dispatch blocked dependants.

The executable backlog replaces scattered TODO lists. Milestones remain in the plan; a task can be complete while its milestone still has outstanding tasks. `next` reports eligible work, not an instruction to execute every task automatically.

## Handoff packet

Use [the task template](../tasks/TEMPLATE.md). Give the agent only the relevant contract and files. Preserve explicit command outputs when a conclusion depends on them; summarize routine successful checks. After two failed repair attempts, send evidence back to the lead instead of extending the retry loop indefinitely.

The initial model pairing is a practical trial. Measure accepted changes, corrections and wall time; compare cost only when actual usage is available. Never claim efficiency solely because the agent used a smaller model.

## Daily verification

`node tools/tasks.mjs check` validates IDs, statuses, ownership, dependencies, cycles, done prerequisites, and evidence paths. `node --test` runs foundation tests. Validate the prototype through real browser interactions after behavioral changes. When Rust lands, add native build/test checks and only the useful additional CI matrix.

GitHub CI is prepared but cannot be described as passed until a remote run occurs. Publishing a remote repository remains separate from organizing this local project.
