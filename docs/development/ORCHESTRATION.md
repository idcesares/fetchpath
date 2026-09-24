# Building Fetchpath with agents

Revision 0.4 · 24 September 2026 · Lead: the user-selected strongest model in Codex or Claude Code; bounded tasks delegated per the tier table below

## Responsibility and model routing

The lead model owns the product contract, architecture, work decomposition, interpretation of benchmarks, and integration. Smaller models implement explicit contracts; they do not independently redesign integrity or persistence behavior. Use these as roles, not a permanent fleet of agents.

| Work | Model tier | Required evidence |
|---|---|---|
| Architecture, trust rules, persistence ordering, scheduler design | Lead/highest-capability model | Decision, alternatives, invariants, and an experiment or review method |
| Product journeys, accessibility, interaction design | Lead model defines UX; capable implementer builds agreed flows | User-flow prototype, keyboard/accessibility checks, clear error and recovery states |
| Bounded production slice with existing interfaces | Economical coding model that passes a calibration trial | Working behavior, relevant tests, concise diff, known limitations |
| Documentation, fixture generation, mechanical edits | Smaller fast model | Exact requested output plus appropriate validation |
| Security-sensitive code, unsafe/FFI changes, recovery races | Strong reasoning model | Independent failure-case review and reproducible tests |
| Benchmark execution and formatting | Scripts; small model only to assist | Raw observations, environment manifest, commands, hashes |
| Final integration and release decision | Lead model | Acceptance matrix checked against actual evidence |

Do not infer competence from model labels or price. Calibrate on a small set of representative tasks: a bounded parser, one CLI behavior, a fixture, a regression repair, a persistence change, and a documentation update. Record first-pass acceptance, rework, total latency, and actual cost if available. Choose by cost per accepted change rather than tokens alone. The expensive failure is repeated plausible but incorrect work.

The running downloader uses deterministic policies and measured feedback. No LLM call belongs in the byte-transfer path. An optional future assistant UI would be a separate product decision.

UX and engine work may proceed alongside each other once the job/event contract is defined. The lead protects coherence between them: one queue, consistent states, and progressive disclosure of advanced controls. Browser integration and media adapters need separate bounded investigations before UI promises imply unsupported behavior.

## Model tiers per tool

The backlog's `modelTier` names a role; the model behind it depends on the tool in use. Record which model actually did the work in the task record.

| `modelTier` | Role | Codex | Claude Code |
|---|---|---|---|
| `astra-high` | Lead: contracts, persistence, security, integration | Astra High | Opus |
| `terra-medium` | Bounded implementation against frozen interfaces | Terra Medium | Sonnet |
| (mechanical) | Docs, fixtures, formatting | smaller fast model | Haiku |

`review: "strong"` in a task means an independent reviewer on the lead tier, not the implementer, derives failure cases from the contract before the task is done.

## Work loop

1. Lead reads project intent, current implementation, and relevant decisions. Reconcile stale notes with files before dispatch.
2. Write a small work packet around one observable behavior. Freeze the necessary interfaces and invariants first.
3. Delegate only independent tasks. Start with at most two implementers alongside the lead; add an independent reviewer when useful. Dependent storage and resume work should usually stay sequential.
4. Each implementer works in an isolated Git worktree or a clearly disjoint file area. One owner changes shared contracts, dependency locks, and migrations at a time.
5. Run the narrow behavioral checks, then lead/reviewer examines the change and targeted failure paths. A model's claim of completion is not evidence.
6. Integrate one slice, run the affected integration checks, record the result, and issue the next packet. A changed API returns to the lead before dependent work continues.

Escalate after two unsuccessful repair attempts, ambiguous identity semantics, unexplained benchmark variance, or any proposed change to a protected invariant. Do not spend unlimited smaller-model retries on a structural problem.

## Work packet template

```text
Outcome: the exact user-visible behavior this task delivers
Prerequisites: relevant milestone, current commit, established interfaces
Inputs: small set of source files and decision/specification excerpts
Ownership: permitted files; shared files requiring coordination
Contract: inputs, outputs, errors, cancellation, resource budgets
Protected invariants: applicable acceptance IDs from ../architecture/PLAN.md
Exclusions: capabilities deliberately outside this task
Verification: fixture, commands, expected observations, failure cases
Return: diff summary, actual check results, limitations, next dependency
Escalation: uncertainty or failure that requires lead intervention
```

For example, M2's resume packet must cover server replacement, range refusal, process interruption, and partial writes. It cannot be decomposed into unrelated networking and database changes until the write/commit contract exists.

## Repository records when implementation begins

Use `PROJECT.md` for purpose and current phase; the implementation specification for behavior; short decision records for consequential choices; and a work log linking slices to actual validation artifacts. Keep raw benchmarks separate from narrative summaries. Preserve evidence paths and verification dates; exclude credentials and private download URLs.

Retain the supplied project rules in the repository's `AGENTS.md`. For code discovery, check graph status and reuse its index; index only the active repository if necessary. Prefer graph tools, then available symbolic tools, then bounded searches. Do not index home directories or dependency trees. These rules are relevant once a code repository exists.

## Verification gates

Every production slice needs the relevant behavioral acceptance checks. Before the first release, require fault-injection coverage, adversarial parser tests, credential-boundary checks, native packaging validation, and measured performance on the supported systems. Track known failures explicitly.

Review independence matters most for high-risk changes. A second agent should derive adversarial cases from the contract, rather than merely repeat the implementer's explanation. Routine documentation or cosmetic edits do not need an elaborate agent ceremony.

The foundation used two Terra Medium agents with disjoint ownership: UX prototype and benchmark fixtures. Lead integration review found and corrected media-choice propagation and missing If-Range behavior. No token-cost comparison was measured. Follow WORKFLOW.md and the backlog for continuing work.
