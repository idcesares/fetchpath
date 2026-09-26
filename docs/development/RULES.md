# Smart rules

FP-064, 26 September 2026. Rules by site, file type or size choose where a
new download is saved, the video quality, whether a checksum is required and
how many connections it opens. The engine keeps and applies them, so every
client and principal gets the same outcome.

## What exists

- **Model** (`fetchpath-protocol` `model.rs`): a `Rule` is an engine-given id
  plus `when` (domains, which also match subdomains; file types; inclusive
  minimum and maximum size) and `then` (folder, media quality `best` / `audio`
  / a height, `require_checksum`, `max_connections` 1–8). A rule needs at
  least one condition and one action. Rules are tried in order; the first
  whose every condition holds decides, and nothing is merged across rules. A
  size condition never matches an unknown size.
- **Engine** (`fetchpath-session` `rules.rs`, `lib.rs`, `engine.rs`): rules
  persist in `settings-v1.json` under `rules`, written only when there are
  some; an unreadable or invalid rule is dropped alone and the load reports
  a repair. `ListRules`, `AddRule` and `RemoveRule` are the person's only.
  `InspectLink` returns a verdict: the matching rule and, for each rule tried,
  why it did or did not match. On `CreateJob`/`CreateJobs`, before an agent's
  grant is checked, a destination that is only a file name is placed in the
  matching rule's folder or the default folder, and a file job a matching
  rule requires a checksum for is refused with `integrity.checksum_required`.
  An agent's job placed outside its grant waits for approval like any other.
  The sizes of the last 64 inspected links are remembered, so a size rule
  decides at creation as it did on the card. A rule's connection cap is
  applied when the job starts, as the adaptive transfer's maximum.
- **Command line** (`apps/cli/src/rules.rs`): `fetchpath rules [list | add |
  rm ID | test LINK]`. `fetchpath add` and `download` without `--to` send only
  the file name, so the engine places it. `add` looks at each link first,
  which also means a video page gets the rule's quality, else the best up to
  1080p, and is never saved as the page; `batch` does not look first. A
  height quality takes the tallest video up to it when no format has that
  exact label.
- **Terminal**: the confirmation card uses the matching rule's folder and
  quality, names the rule and why it matched, and will not start a file a
  rule needs a checksum for; `/rules` takes the same words as the command.

## Verification

- `rules::tests` (first match, reasons, domain boundaries, validation,
  load-time sanitizing) and `settings::tests` (round trip, an unreadable rule
  dropped alone) in `fetchpath-session`; the protocol round trip and schema.
- `smart_rules_place_and_check_new_jobs_for_every_principal_and_an_agent_stays_in_its_grant`
  (`fetchpath-session/tests/policy.rs`): placement by rule, an agent's job
  held outside its grant, the checksum refusal for the person and an agent,
  the verdict on `InspectLink`, agents refused rule changes, rules kept across
  an engine restart.
- `rules_place_downloads_explain_themselves_and_can_require_a_checksum`
  (`apps/cli/tests/queue.rs`) through a real engine: `rules add`, `rules
  test`, a size rule placing `add`'s download, a refused `.iso`, `rules rm`.
- Headless ConPTY walkthrough, 26 September 2026: an `.iso` card named
  "Rule 1 (Disc images): the file is a .iso, 2.0 MiB is at least 1.0 MiB",
  showed the rule's folder and saved there; a `.zip` card named Rule 2 and
  refused to start without a checksum; `/rules test` listed both rules with
  their reasons.

## Limitations

- The connection cap reaches the transfer's limit but is not separately
  measured: the adaptive transfer starts at one connection and grows with
  timing.
- The desktop app does not show or edit rules yet; its downloads follow them
  only when it sends a bare file name, which it does not.
- An agent may read the verdict on `InspectLink`, including the matching
  rule's folder; it can never change rules.
- A rule's folder applies only when no folder was given. Rules are not
  reordered in place; remove and add with `--position`.
- `integrity.checksum_required` maps to the checksum exit code (5) on the
  command line.
