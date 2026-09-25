# Retired records

Removed on 24 September 2026 (FP-071) because no open task, contract or
release step reads them. Each is intact at commit `5d7299f`; read one with
`git show 5d7299f:<path>`. Finished tasks whose evidence lived here cite this
page.

| Path | Task | What it was |
|---|---|---|
| `docs/development/FOUNDATION-RESULTS.md` | FP-001–003 | Foundation checks and the retired desktop prototype |
| `docs/development/HTTP-BACKEND-SPIKE.md`, `evidence/http-backend/`, `evidence/baseline-http1.json` | FP-004 | libcurl backend spike; decision now in `docs/architecture/PLAN.md` |
| `docs/development/JOB-CONTRACT-REVIEW.md` | FP-005 | Review of the job contract; the contract is `docs/architecture/JOB-CONTRACT.md` |
| `docs/development/BROWSER-HANDOFF-SPIKE.md`, `evidence/browser/browser-spike.json` | FP-006 | Browser handoff spike; production record is `BROWSER-CAPTURE.md` |
| `docs/development/MEDIA-SPIKE.md`, `evidence/media/run.jsonl` and helper version files | FP-007 | Media helper spike; production record is `MEDIA-INTEGRATION.md` |
| `evidence/ui/framework-spike.json` | FP-008 | Tauri 2 vs WinUI 3 raw runs; decision is `docs/architecture/DESKTOP-FRAMEWORK.md` |
| `docs/development/FIRST-DOWNLOAD.md`, `FIRST-DOWNLOAD-REVIEW.md` | FP-009 | First native download and its review, superseded by later records |
| `docs/development/RELEASE-POLISH.md` | FP-026–030 | 0.1.0 polish record |
| `docs/development/RELEASE-READINESS.md` | FP-031, FP-035–039 | 0.1.0 readiness record; the gate is `RELEASE-CANDIDATE.md` |
| `docs/development/ORCHESTRATION.md` | — | Model routing; the part still in force is in `WORKFLOW.md` |
| `docs/development/plans/` | FP-020, FP-048–053 | Implementation plans of finished tasks |
| `evidence/engine-session/fp-049-walkthrough/` | FP-049 | Desktop before/after walkthrough scripts and transcripts |

## FP-071 result

| Measure | Before | After |
|---|---|---|
| Tracked files | 265 | 232 |
| Tracked bytes | 2.83 MB | 2.52 MB |
| `docs/` | 75 files, 752 KB | 42 files, 451 KB |
| `PROJECT.md` + `ORCHESTRATION.md` + `WORKFLOW.md` | 19.9 KB | 8.6 KB |
| `target/` | 45.3 GiB | rebuilt on demand |
| `work/` | 1.4 GB | empty |
| Local branches / worktrees | 8 / 2 | 1 / 1 (all were merged) |

Tests were reviewed and kept: each guards a contract, persisted format or
invariant, and the suites stay fast (326 Rust tests pass with 8 ignored,
235 s including a cold build; 39 Node tests in 8 s). Done tasks' verification
text in the backlog was left as it is, because `tasks.mjs show` reads one
task at a time. The rules that stop this piling up again are in `AGENTS.md`
("Keep the tree small") and [the workflow](WORKFLOW.md).
