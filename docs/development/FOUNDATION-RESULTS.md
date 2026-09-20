# Foundation validation — 19 September 2026

Scope: FP-001 repository/task organization, FP-002 simulated UX prototype, FP-003 local fixture/baseline. M0 as a whole remains open. Commands below were run on native Windows; timestamps in raw logs use UTC, which may show 20 September.

## Observed environment

- Git 2.55.0.windows.3; Node v24.20.0.
- Rust 1.98.1 and Cargo 1.98.1 found under `%USERPROFILE%\.cargo\bin`; absent from session PATH. A minimal Rust executable was compiled and run successfully from ignored `work/`. This proves the basic compiler/linker path, not libcurl or GUI packaging.
- System curl 8.21.0 with Schannel. Its advertised features contain neither HTTP2 nor HTTP3. FP-004 must validate the actual packaged production backend.
- Graph service returned Transport closed. Serena activated this repository and provided symbolic review of the fixture and runner. No home-directory index was created.

## Passed checks

| Check | Evidence and result |
|---|---|
| Fixture and task-tool tests | `node --test`: 10 passed, 0 failed. Covers size bounds, valid/invalid ranges, ignored ranges, current/stale/weak If-Range, changed content, truncation, task dependencies/cycles, active ownership and evidence boundaries. |
| Prototype syntax | `node --check prototypes/desktop/app.js`: passed. |
| Native Rust probe | `rustc work/toolchain-probe.rs -o work/toolchain-probe.exe` using the explicit installed rustc path, followed by execution: passed. |
| Baseline smoke | `node tools/bench/benchmark.mjs --output-dir work/bench --repetitions 2 --size 1048576`: two curl HTTP/1.1 200 responses, each 1,048,576 bytes and independently hash-matched. Retained raw snapshot: [fixture baseline](evidence/baseline-http1.json). |
| Browser walkthrough | In-app browser at loopback prototype: advanced view, pause/resume, completed filter, add dialog, Escape dismissal/focus return, audio quality selection and queue insertion observed. Corrected High/MP3 choice visibly produced Source-audio.mp3 with 95 MB. Simulation labels remained visible. |

The lead reviewed both delegated outputs. Review found missing If-Range fixture behavior and a media-choice size/extension mismatch. Both were corrected before closing the foundation tasks. The two implementation agents used Terra Medium; token/cost savings were not measured.

Final integration: `node tools/tasks.mjs check` passed for 25 tasks, and the local Markdown link check found 18 links with none broken. The executable backlog links this record and does not claim production acceptance A01–A12 is complete.

## Limits

This is a localhost HTTP/1.1 sanity result, not a performance comparison. No H2/H3 transfer, remote-source benchmark, production download core, real media extraction/muxing, browser-extension capture, power-loss durability, native desktop packaging, or full accessibility audit has passed. The UX prototype is simulated, uses illustrative content and has no persistence. GitHub CI is prepared but has not run remotely.

## Next ready work

Run `node tools/tasks.mjs next` for authoritative readiness. FP-004 (backend packaging), FP-005 (job/event contract), FP-006 (browser spike), FP-007 (media spike), and FP-008 (desktop framework spike) are the next bounded investigations. Their outputs determine the first real desktop download slice.
