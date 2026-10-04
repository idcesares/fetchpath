# Torrents and magnets (FP-021)

The engine owns a torrent job in the same queue and starts an isolated
`fetchpath-torrent-helper.exe` beside its executable. The helper uses pinned
`librqbit` 9.0.1 (Apache-2.0), consumes one bounded JSON request, and reports
bounded progress and terminal events. A torrent a person submits discovers
peers by default and is refused if discovery is turned off, since it could not find peers; uploading is off by default and requires
a separate choice. Agents wait for approval for discovery or upload. Browser captures cannot start a
torrent. The helper caps peers at 32, download at 32 MiB/s and upload at
128 KiB/s, and does not listen for incoming peers.

Magnet links, HTTPS metadata links, and local `.torrent` files are accepted.
The desktop and terminal add flows select Torrent for these inputs; peer
discovery is on; clearing the desktop checkbox is refused by the engine. The engine snapshots local metadata (at most 4 MiB)
under its private data directory before saving a v2 queue record. Restart
rehashes that copy and fails closed if it is missing or changed; replacing a
local source with a new link retires the old snapshot after the queue save.
V1 queue files remain readable. Metadata downloads are limited to 4 MiB;
torrent file names are checked against Windows path and
device-name hazards. The transfer stays in a sibling staging folder, the
helper waits for piece verification, checks for reparse points, then renames
the folder to a new destination. Completion is reported after publication.
An agent's byte limit is also passed to the helper. Cancellation kills the
helper and leaves staging for a later retry.

Build evidence for the first implementation: `cargo check --workspace --locked`,
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

The 29 September 2026 clean-Sandbox report showed the packaged helper could
not start without `VCRUNTIME140.dll`. Its Windows MSVC helper build now links
that runtime statically, and a PE-import regression test covers both the
release and staged helper. The rebuilt installer passed the automated
clean-Sandbox lifecycle, where the helper started without the redistributable,
and the owner added torrents from a magnet, an HTTPS `.torrent` link and a local
`.torrent` file in the desktop, command line and terminal of that clean install.

FP-089 (1 October 2026): a torrent no longer needs a destination. An empty
destination is automatic: the engine resolves the root through the person's
rules and default folder (`Session::resolve_torrent_destination`), the queue
record keeps the root and an automatic flag across restarts, and the helper
stages in `<root>\.fetchpath-<job>-<hash>.part`, names the folder from the
torrent's info name (hazardous names fall back to `Torrent <hash prefix>`) and
publishes to `name`, `name (2)`, and so on with Windows `MoveFileExW`
without replacement. An existing destination is never overwritten. The
publication marker remains until the engine has durably saved completion.
The view can show the published folder immediately, while a Running queue
record retains the original root and automatic flag so a restart can find
that marker. Only Completed promotes the persistent destination. A crash
after the durable save but before marker cleanup can leave a harmless marker.
A
bare name goes into the same root; a full path behaves as before. An agent's
automatic destination is checked against its grants as a folder inside the
root. Verified by session, policy and helper unit tests; the packaged helper walkthrough passed; release boundary review found and
repaired publication ordering and Windows no-overwrite behavior.

Peer discovery default (FP-090): the protocol's `discover_peers` is optional; absent means on for a person and off for an agent, and the engine stores the effective policy in the record so retry and restart reuse it. A torrent whose effective discovery is off (a person clears it, or an agent omits or sends false) is refused at creation with `policy.discovery_off`; the helper has no trackers or DHT, so queueing it could only fail. An agent is told to request `discover_peers`, which needs approval. Cancel kills the helper process, which ends all peer activity; there is still no pause. Verified by protocol wire, session policy and CLI parsing tests; the installed-app walkthrough and independent strong review are still open.

Current limits: choosing only the root for an automatic torrent is not
offered (a full path is an exact new folder), no client previews the resolved
root before submission, and torrent jobs cannot be
paused in place. A magnet's private source may need to be supplied again
after an engine restart. There is no per-file selection or seeding after
completion. Peer traffic and untrusted torrent metadata remain subject to
the dependency's implementation; future releases should exercise this path
with representative swarms before publication.
