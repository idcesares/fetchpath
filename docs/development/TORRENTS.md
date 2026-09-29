# Torrents and magnets (FP-021)

The engine owns a torrent job in the same queue and starts an isolated
`fetchpath-torrent-helper.exe` beside its executable. The helper uses pinned
`librqbit` 9.0.1 (Apache-2.0), consumes one bounded JSON request, and reports
bounded progress and terminal events. The person explicitly enables peer
discovery; uploading is off by default and requires a separate choice. Agents
wait for approval for discovery or upload. Browser captures cannot start a
torrent. The helper caps peers at 32, download at 32 MiB/s and upload at
128 KiB/s, and does not listen for incoming peers.

Magnet links and HTTPS metadata links are accepted. Metadata downloads are
limited to 4 MiB; torrent file names are checked against Windows path and
device-name hazards. The transfer stays in a sibling staging folder, the
helper waits for piece verification, checks for reparse points, then renames
the folder to a new destination. Completion is reported after publication.
An agent's byte limit is also passed to the helper. Cancellation kills the
helper and leaves staging for a later retry.

Build evidence: `cargo check --workspace --locked`,
`cargo check -p fetchpath-torrent --features helper --locked`, desktop
`pnpm exec tsc --noEmit`, and the package staging build. The owner waived the
optional torrent test campaign on 28 September 2026. This is unreleased work;
the 0.1.0 installer does not contain the helper.

Commit-tree verification on 29 September fixed a missed persisted-record test
field, and a review found two retry boundaries: refreshing an agent torrent now
asks again for peer discovery and upload approval, and staged files are tied to
the source and torrent identity before reuse. The targeted session and helper
tests cover these cases. The protocol v1 schema was regenerated for the torrent
job and approval reasons; `cargo test --workspace --locked`, the helper tests,
and `cargo fmt --check` passed.

Current limits: torrent jobs need a new destination folder and cannot be
paused in place. A magnet's private source may need to be supplied again
after an engine restart. There is no per-file selection or seeding after
completion. Peer traffic and untrusted torrent metadata remain subject to
the dependency's implementation; future releases should exercise this path
with representative swarms before publication.
