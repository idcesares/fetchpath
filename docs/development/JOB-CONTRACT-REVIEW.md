# FP-005 contract review

Date: 20 September 2026  
Scope: [job, identity, and event contract](../architecture/JOB-CONTRACT.md)  
Acceptance: A02, A03, A04 architecture gate

An independent Astra High / high-reasoning read-only review derived adversarial cases from the contract. It initially held the gate for six issues, then re-reviewed each correction. The final review passed with no remaining P1/P2 blockers. This validates the contract's internal architecture, not a future implementation.

| Finding | Resolution in the accepted contract |
|---|---|
| Command retry could duplicate a job after a crash | One transaction now records identity, fingerprint, server receipt, mutation, events, and immutable result; mismatched payload reuse fails |
| Revoked work could still overwrite through a queued writer | `work_generation` and reservation fences follow data through validation, writer execution, and checkpoint commit; overlaps wait for quiescence |
| Harmless policy revisions could discard valid active work | `job_revision` and `work_generation` are separate; only identity/execution changes advance the latter |
| Pause/cancel/restart states and publication races were incomplete | Authoritative matrix covers all states; publication and pause/cancel share a serialized gate; recovery reconciles possible publication first |
| Expired URLs and vanished media selections lacked command paths | Source refresh revalidates identity; unfrozen media choices can refresh; frozen format changes create linked replacement jobs |
| Durable event order conflicted with progress coalescing | Durable `seq` is separate from droppable `sample_cursor`; subscription defines an atomic snapshot/replay boundary |

Follow-up review also required replay-window retention tied to future clock skew and a UUID-form client identifier. Both are present in the accepted version.

Implementation review remains mandatory for command atomicity, late-callback fencing, destination races, checkpoint order, publication recovery, and the test cases listed in section 13 of the contract.
