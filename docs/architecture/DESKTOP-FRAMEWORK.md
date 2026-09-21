# Desktop framework and supported Windows decision

Status: accepted for FP-008 · 20 September 2026

## Decision

Use **Tauri 2** for the production desktop shell in `apps/desktop`. Keep transfer, recovery, persistence, policy and secret-bearing work in the Rust core. The webview is a local presentation client over narrow typed commands and events; it does not fetch payload bytes, host remote pages or own durable job state.

Start with the smallest practical TypeScript renderer and Tauri's first-party window/tray APIs. The renderer library, if one becomes useful, is a reversible implementation choice rather than part of this decision. Ship an x64 NSIS installer first, using the system Edge WebView2 runtime and its downloaded bootstrapper fallback. Signing, update policy and public distribution remain later release gates.

The supported desktop target is **Windows 11 x64 on releases still receiving Microsoft servicing**. At this decision date the floor is Windows 11 24H2 (build 26100), with 25H2 (build 26200) as the primary development and QA target. Microsoft lists 24H2 support through 14 October 2026 and 25H2 through 13 October 2027, so the floor advances to 25H2 when 24H2 leaves servicing. Windows 10, x86 and Windows on ARM64 are not launch-supported targets. ARM64 may be added only after the Rust core, native helpers, installer, browser host and updater pass native ARM64 testing. [Windows 11 lifecycle](https://learn.microsoft.com/en-us/lifecycle/products/windows-11-home-and-pro)

This is a product-support boundary, not a claim that Tauri cannot execute on older Windows versions. Windows 10 22H2 reached ordinary support end on 14 October 2025; optional ESU enrollment does not make it an appropriate default support floor for a new consumer application. [Windows lifecycle FAQ](https://learn.microsoft.com/en-us/lifecycle/faq/windows)

## Compared candidates

The spike compared exactly two credible candidates: Tauri 2 and WinUI 3. Both implemented the same add-download, queue, pause/resume, advanced-options and close-to-tray journey. Both built and ran on the reference machine. The raw runs and environment are in [framework-spike.json](evidence/ui/framework-spike.json).

| Boundary | Tauri 2 | WinUI 3 |
|---|---|---|
| Accessibility | Semantic HTML appeared in Windows UI Automation as a document with a labeled edit, buttons, status text and groups. Renderer accessibility was forced on for a deterministic probe. | Native controls produced the cleanest UI Automation tree, including named edit, button, progress and expander patterns. |
| UIA-ready startup, five-run median | **666 ms** | 982 ms |
| Two-second process-tree working set, median | 382.19 MiB across 7 processes | **148.09 MiB in 1 process** |
| Two-second process-tree private bytes, median | 169.51 MiB | **76.94 MiB** |
| Tray lifecycle | First-party tray/menu APIs; close-to-tray passed after retaining the tray handle and preventing natural last-window exit. | Close-to-tray passed, but required direct `Shell_NotifyIcon` and window-subclass interop for a dependency-free spike. |
| Native/core integration | Rust host can consume the Rust core directly with one lifecycle and build graph. | The C# path needs a reviewed C ABI/FFI or out-of-process protocol to reach the Rust core. C++ is possible but the current official learning/tooling path is less direct. |
| Built installer | **1,348,469-byte NSIS setup**, using the shared WebView2 runtime/bootstrapper model | 30,471,143-byte unsigned self-contained MSIX; packaged development launch required Developer Mode, so the spike ran unpackaged for measurements |
| Local toolchain | Existing Rust, MSVC, Node and pnpm were sufficient | Required a project-local 286.6 MB .NET 10 SDK and WinUI templates because no .NET SDK/workload was installed globally |

Working-set totals are process sums and can count shared pages more than once. Private bytes are included because neither number alone is a perfect unique-memory measure. A single five-second hidden-window sample did not reduce either candidate's footprint: 164.47 MiB private for Tauri and 77.32 MiB for WinUI. Treat these figures as local comparative evidence, not general product claims.

WinUI 3 is Microsoft's native Windows desktop UI framework and supports packaged and unpackaged distribution. It remains the stronger candidate for native control behavior and resident memory. [WinUI 3 overview](https://learn.microsoft.com/en-us/windows/apps/get-started/winui-get-started-overview) Tauri won this decision because it reached an accessible, responsive, installable shell with the existing Rust direction, avoided a permanent FFI/process boundary around the core, exposed a much smaller online installer, and had a faster UIA-ready median. Tauri's higher renderer footprint is a real cost, not hand-waved away.

Tauri's documented Windows prerequisites are MSVC C++ build tools and WebView2, both already present here. Its Windows bundler supports NSIS setup executables and MSI; the default installer downloads the WebView2 bootstrapper only when the runtime is absent, and Windows 11 distributes WebView2 with the operating system. [Tauri prerequisites](https://v2.tauri.app/start/prerequisites/), [Tauri Windows installer](https://v2.tauri.app/distribute/windows-installer/)

## Production constraints

The first desktop vertical slice must preserve these boundaries:

- Keep URLs with secrets, cookies, authorization material and private paths out of the DOM, logs and telemetry. Pass opaque identifiers wherever the UI does not need the value.
- Disable arbitrary navigation and remote content. Replace the spike's permissive CSP with a production CSP and grant only task-specific Tauri capabilities.
- Keep closing the window distinct from stopping downloads. The Rust coordinator and tray survive window close. Evaluate destroying and recreating the renderer while hidden if normal queue usage does not meet an explicit memory budget.
- Use semantic elements, visible focus, non-color status, system light/dark behavior and reduced-motion handling. Windows UI Automation inspection is necessary but not sufficient: release QA still includes keyboard-only operation, Narrator, Accessibility Insights, contrast themes and 200% scaling. Microsoft's accessibility guidance calls for names/roles/values, keyboard navigation, contrast/text scaling and testing with Narrator and inspection tools. [Windows accessibility guidance](https://learn.microsoft.com/en-us/windows/apps/develop/accessibility)
- Treat WebView2 as a presentation dependency, never as the download engine. Progress must be throttled and large queues virtualized.
- Produce signed release artifacts and verify install, update, rollback and uninstall on clean supported Windows images before satisfying A12. The unsigned spike artifacts are not distributable releases.

## Revisit triggers

Reopen the framework decision only with new evidence: failure of the production journey under Narrator/keyboard/contrast testing; inability to keep normal hidden and visible memory within an agreed budget after renderer-lifecycle work; an unresolvable WebView2 security/capability boundary; or materially simpler Rust integration from a native alternative. Aesthetic preference or a synthetic microbenchmark alone is not enough.
