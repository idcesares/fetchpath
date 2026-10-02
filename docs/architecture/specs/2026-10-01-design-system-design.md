# Design system and visual identity (FP-094)

Status: proposed, 1 October 2026. Tokens: [`design/tokens.css`](../../../design/tokens.css). Review compositions (local, ignored): `work/design/fp094-compositions.html`, built by inlining the token file. Contract it must stay consistent with: [`docs/product/UX.md`](../../product/UX.md).

## Direction: "the trail"

Fetchpath keeps its forest-teal ground and mint accent and adds one signature: a **trail**. A download is a path with a leading waypoint, so progress is a track with a dot at its head, a row has a status rail on its left edge, and a destination reads as a path of crumbs (instance, folder, leaf). The tone is calm, exact and quiet: no gradients on controls, no decorative motion, numbers in tabular figures, paths in a monospace face. Color is spent on state, not decoration; the app background keeps one soft mint glow at the top left (existing).

What changes from today: a semantic token layer (names say what a color means, not what it is), a distinct approval role (violet), an info role (blue), status glyphs on every state, a 4px left rail on rows, the waypoint dot on progress, instance chips, and a high-contrast theme of our own beside OS forced-colors.

## Source of truth and ownership

- `design/tokens.css` is the only place a color, size, radius, shadow or duration is defined. Owner: the UX owner; lead reviews. Changing a token name or meaning is a contract change (note it in the task record); changing a value is not.
- Desktop imports it (`@import` from `apps/desktop/src/styles.css`, or a Vite import; the bundler already exists). The extension copies it at build into `extensions/browser` (a copy step in its build, never a hand-edited fork); the popup then uses the same names.
- Components use only `--fp-*` names. The legacy block at the end of the file maps today's `--accent`, `--muted`, `--line` and the rest so `styles.css` and `popup.css` keep working until FP-095 removes it.
- The terminal does not read CSS. Its six roles map from semantic roles by the table below; that table is the contract, and `apps/cli/src/tui/config/theme.rs` stays the implementation.
- Fonts are system faces (`Segoe UI Variable`, `Cascadia Mono`), so no font license or bundle is needed. Icons are a small inline SVG set drawn on a 24px grid, 2px stroke, round caps, `currentColor` (own artwork, no icon library). Shipped icons are listed with the app icons in `apps/desktop/src-tauri/icons`; extend that set rather than adding a library.

## Tokens

Names below drop the `--fp-` prefix. Values for all three themes are in the file.

| Group | Tokens | Notes |
|---|---|---|
| Surface | `bg`, `surface`, `surface-raised`, `surface-inset`, `surface-hover`, `scrim` | Raised is for dialogs; inset for chips, code, wells |
| Line | `line`, `line-strong`, `field`, `field-line`, `track` | `field-line` is at least 3:1 against `field` (an input boundary is non-text UI) |
| Text | `text`, `text-muted`, `text-quiet` | All at least 4.5:1 on `surface` and `bg` in every theme |
| Accent | `accent` (fills), `accent-strong` (hover, leading dot), `on-accent`, `accent-text` (text and glyphs on surfaces), `focus` | Separate fill and text roles: a fill that suits a button is not always legible as text |
| Status | `running-*`, `done-*`, `waiting-*`, `paused-*`, `failed-*`, `approval-*` (each `fg`, `bg`), `danger`, `warning`, `info` | `fg` on `bg` at least 4.5:1; fg also drives the row rail and glyph |
| Type | `font-sans`, `font-mono`, `text-xs`..`text-2xl`, `weight-regular/strong/heavy`, `leading`, `track-caps` | Rem-based so 200% text scaling works; caps only for eyebrows |
| Space | `space-1`..`space-7` | 4px base: 4, 8, 12, 16, 24, 32, 48 |
| Size | `icon`, `icon-sm`, `rail`, `trail`, `measure`, `shell`, `popup`, `dialog`, `control-h` | `rail` is 4px (6px in high contrast) |
| Radius | `radius-sm` 6, `radius-md` 10, `radius-lg` 16, `radius-pill` | Controls md, cards lg, chips pill |
| Elevation | `shadow-1` (cards), `shadow-2` (dialogs, popup) | Two levels only; none in high contrast (borders carry the edge) |
| Density | `control-h`, `row-pad`, `row-gap` | `:root[data-density="compact"]` sets 32px, 8px, 6px; comfortable is 40, 14, 10. No target below 24px |
| Motion | `ease`, `dur-fast` 120ms, `dur` 200ms, `dur-slow` 400ms | Used for hover, progress width, dialog open. Reduced motion sets all to ~0 |

Themes: dark is the default identity; light uses the existing light palette with the accent fill darkened to `#19745f` so white text passes; `high-contrast` (`data-theme`) is black ground, white lines, 7:1 text, bright status colors and thicker rails. `system` follows `prefers-color-scheme`; `forced-colors: active` replaces colors with system keywords and keeps rails, borders and glyphs.

Computed contrast, all pairs, is generated in the review file and recorded at the end of this document.

## Components

Each has a state set: default, hover, focus-visible (3px `focus` ring, 2px offset, never removed), disabled (opacity .55 plus `aria-disabled`/`disabled`), and where relevant invalid and busy.

- **Button**: min height `control-h`, radius md. Primary (accent fill, one per view or dialog), secondary (outlined), danger (outlined `danger`, never filled; the confirming dialog says what is lost). Icon plus text; icon-only buttons need `aria-label` naming the target (for example "Pause ubuntu.iso").
- **Input**: `field` ground, `field-line` border, label above (always visible, never placeholder-only), hint below in `text-muted`. Paths and magnet links use `font-mono`. Invalid: `danger` border, `aria-invalid`, message linked with `aria-describedby` and a glyph.
- **Checkbox / switch row**: native checkbox 20px with `accent-color`; the whole label is the target; a consequence sentence sits under the label (for example the IP note).
- **Dialog**: `surface-raised`, radius lg, `shadow-2` over `scrim`; width `dialog`. Heading names it; `role="dialog"` (`alertdialog` for approvals); focus moves in, is trapped, Escape cancels, focus returns to the opener. Actions at the end: cancel left of the primary.
- **Navigation**: pill tabs in the app bar, current page has a filled hover ground plus a 2px accent underline and `aria-current="page"`. Order: Queue, History, Rules, Settings. Below 640px the bar wraps; nothing hides behind an icon-only menu.
- **Queue row**: left rail in the status `fg`, name (truncates, full name on focus/hover title), status chip (glyph plus word), row actions right, trail, meta line (percent, size, speed, time left, tabular figures), destination crumbs in mono when more than one instance exists. Failed rows add the error block; completed rows state what was checked and how.
- **Progress**: the trail. Track `track`, fill `accent`, head dot `accent-strong`. Paused is a dashed fill with a gray dot; failed is `danger`; done is `done-fg` without a dot. Native `progressbar` role with name, value, min, max. Width animates in `dur-slow` only without reduced motion.
- **Status**: always glyph plus word plus color. Glyphs: running arrow, completed check, queued clock, paused bars, failed triangle, approval shield. A chip is never color alone.
- **Error**: left bar plus glyph plus a plain sentence, then what is safe, then the next action. Never a raw code as the headline; details are one click away.
- **Empty state**: dashed container, mark, one sentence of what to do, one primary action, one sentence of reassurance. No illustration beyond the mark.
- **Approval**: eyebrow in `approval-fg`, who is asking and from which instance, destination crumbs, what happens on each choice, approve once as primary, deny as danger-outline. Verification text says "computed here" and never implies publisher authenticity.
- **Instance identity and destination hierarchy** (for remote clients). Every instance has a name chosen by the owner and a two-letter monogram. Local is a circle on `accent`; a remote instance is a rounded square on `info`, so the two stay apart without color alone. Destination is always read left to right as `instance › root folder › leaf`, last crumb emphasized with an underline. Single-instance setups hide the instance crumb and the chips. Any action that crosses instances (approve, add to, delete) names the instance in its button or dialog title. A remote client must never show a local path without its instance.

## Terminal adaptation

The TUI draws with six roles (`theme.rs`: accent, dim, good, bad, warn, check) and four themes. It adopts semantic meaning, terminology and hierarchy, not components or hex values.

| Semantic role | TUI role | ANSI (default theme) | Non-color cue |
|---|---|---|---|
| running, accent, selection edge | accent | cyan | `[>]`, progress bar of block characters, reverse video row marker `>` |
| completed | good | green | `[v]`, word "done" |
| queued, waiting, warning | warn | yellow | `[~]`, word "queued" |
| paused | dim | bright black | `[=]`, word "paused", dashed bar |
| failed, danger | bad | red | `[!]`, word "failed" plus reason |
| approval | check | magenta | `[?]`, word "approval", requester named |
| muted, quiet text | dim | bright black | separators and key hints only, never the sole carrier of meaning |
| info | accent | cyan | prose |

Rules: hierarchy is the same (header with instance and totals, rows, hint line); one row per job, bar on the line below only for the selected or running job; reverse video marks the selection and focus. `plain` renders the same glyphs and words with no color, so nothing is lost. The built-in `high-contrast` and `light` themes keep their stated 7:1 and 4.5:1 floors. ASCII fallback for block characters is `#` and `-`. Instance names appear in the header only when more than one instance is configured. No engine crate gains a presentation dependency.

## Accessibility rules

- Everything reachable and operable by keyboard in reading order; focus ring always visible; no keyboard trap except inside an open dialog.
- Every control has an accessible name that includes its target when repeated (rows). Status chips are text, not images; icons are decorative (`aria-hidden`) unless they stand alone.
- Text contrast at least 4.5:1 (7:1 in `high-contrast`); non-text boundaries (focus ring, field border) at least 3:1. The review file recomputes this from the live tokens, in every theme.
- Status is never color alone: glyph, word and position (rail) always accompany it.
- Layout holds at 200% text scale and 320 CSS px width without horizontal scroll; rows wrap their action groups; the review file has a 200% toggle.
- `forced-colors: active` and the `high-contrast` theme keep borders, rails and glyphs; fills never carry meaning alone.
- `prefers-reduced-motion: reduce` removes transitions and animation; nothing essential is conveyed by motion.
- Live regions announce completion and failure politely; approvals are announced assertively.

## Migration

FP-095 (desktop and extension), one reviewable step each, no framework:

1. Import `design/tokens.css` ahead of `styles.css`; add the extension build copy step. Nothing visibly changes (legacy aliases).
2. Replace the dark block (lines ~11-27) and the duplicated light blocks in `styles.css` with the tokens; delete them. Replace `.status` colors with `*-fg/*-bg` and add glyphs. Drop the extension's `prefers-color-scheme` palette.
3. Rows: add the rail and the trail head dot; add instance and destination crumbs only when a second instance exists.
4. Add the `high-contrast` option to the appearance setting and `data-density` setting, persisted with existing settings.
5. Walk the keyboard, 200% and forced-colors paths in the installed app; fix, then remove the legacy alias block.

FP-096 (terminal): add semantic glyph and word cues in `view.rs` where a status is color only, add instance names to the header when several exist, and keep `theme.rs` roles unchanged except tuning the default theme RGB values to the table above if the owner wants parity. Existing `ratatui` only.

Out of scope: a component library, a font or icon package, a screenshot test for decoration. Behavioral tests stay on keyboard, names and status text.

## Decisions (1 October 2026)

1. Violet for approval and blue for remote instances: accepted by the owner, provided they stay in harmony with the green trail.
2. Windows system fonts; no bundled face (owner).
3. The waypoint mark becomes the Fetchpath mark, in the app and as the app and extension icon, applied in FP-095 (owner).
4. `light-dark()` (Chromium 123+, Firefox 120+) is accepted: Fetchpath targets current engines, with no fallback for older browsers (owner).

## Contrast record

Computed from the tokens (WCAG 2.x), minimum over all text and status pairs: dark 5.88 (`text-quiet` on surface), light 4.94 (`text-quiet` on inset), high contrast 12.30. Non-text: focus ring 6.73 or better, field border 3.67 (dark) and 4.02 (light). Full table: review file.
