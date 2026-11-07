# File transfer and sharing: send, inbox and public links

Status: accepted by the user, 31 October 2026 (scope, three slices, reachability modes, approvals; written spec reviewed and approved). Backlog tasks proposed, not yet applied. Proposed home: FP-093, reframed (§9). No code, deployment or network exposure is part of this document.
Extends [the remote hub design](2026-10-03-instance-access-and-remote-hub-design.md) (§1 control and file bytes are separate; S3 retrieval tickets; S4 delivery), [the cache and LAN design](2026-09-22-content-cache-and-paired-lan-design.md) (pairing, credential-free provenance) and [the job contract](../JOB-CONTRACT.md) (D1 principals, `awaiting_approval`; D7 automatic mode).

## 1. Purpose and limits

Fetch brings bytes in with integrity. Send is the reverse direction of the same engine: a person, or an agent under the person's approval, hands a file to someone else. It stays inside Fetchpath as a new job kind, not a separate tool.

Constraints chosen by the user:

- A recipient with only a browser must be reachable (slice 3).
- No vendor or hosted service is required. Fetchpath hosts nothing; whoever shares runs, or chooses, the infrastructure.
- As simple and instructional as possible for the person doing it.

Not in scope: a Fetchpath-operated relay or account service, anonymous public hosting, storing files on a third party, automatic sharing of any kind.

## 2. Slices

| Slice | Delivers | Depends on |
|---|---|---|
| T1 Same-machine inbox | An agent or person offers a published output to a recipient on this machine, with a manifest; the recipient accepts or declines | Nothing blocked: engine, D1 policy and MCP exist |
| T2 Paired devices | Send to a paired LAN device (and later a private-network device) | T1; FP-091 strong review for the shared identity and credentials; S2 for off-LAN |
| T3 Public link | A single-use expiring HTTPS link a browser opens | T1; S3 retrieval ticket semantics; the share server (§5) |

T1 is its own unblocked task. T2 and T3 inherit FP-091/FP-092 where §2 says so. S3 (recipient retrieval over the private network) and T3 share the ticket semantics of §4 but not a listener (§5).

## 3. What can be shared and who approves

Shareable:

- Published job outputs the engine owns.
- A file the person picks with a file picker in the desktop or terminal.
- For an agent: only a published output or a file inside a folder the person granted that agent. Never a path the agent chooses outside its grants, and no filesystem browsing.
- Content whose provenance ever touched cookies, a stored credential or a referer is not shareable by default, mirroring the cache rule (provenance is immutable and cannot become shareable later).

Approvals:

| Case | Approval |
|---|---|
| Person shares from the desktop or terminal | Their action; they choose expiry, uses and size cap |
| Agent offers to the person's inbox (T1) | The person accepts or declines |
| Agent wants a public link or an off-machine send | Always an explicit approval in `awaiting_approval` showing file name, size, hash, expiry and destination. Never covered by automatic mode (D7) |
| Remote web or remote agent principal | Cannot create public links or off-machine sends; no new remote grant is introduced |

A finished download never creates a link or a send on its own (AGENTS.md: sharing is never enabled implicitly).

## 4. Manifest, tickets and integrity

- **Manifest** (every send): file name, size, SHA-256 computed by the sender's engine, origin URL when known, declared sender label (`agent:<name>` is a name registered in the handshake, not proof of identity; same-user malware is out of scope), time.
- **Integrity wording.** The hash is attested by the sender's engine. It never reads as publisher authenticity. The recipient page and inbox show the source of the hash.
- **Ticket** (T3): single-use by default, 24 hour default expiry, raised only by the person. Revocable at any time; a transfer in progress stops. Range resume is supported, bounded bytes and rate, and the engine re-checks the published file is unchanged before serving.
- **Audit**: created, opened, completed, revoked, expired. No recipient identity beyond what the transport shows.

## 5. Share server and reachability (slice T3)

One share server serves `GET /s/<id>`, Range, and the page that decrypts in the browser. It is its own listener and port. It is never the S1b web UI port or the S2 remote port (remote hub spec §5: a gateway cannot be told apart from local traffic).

**End-to-end encryption.** The link is `https://<host>/s/<id>#<key>`. The key is in the fragment, which browsers never send, so a relay or tunnel vendor carries only ciphertext of the file. It cannot read the key passively, but a vendor that terminates TLS also serves the decrypting page and could serve a modified one; see the page-integrity rule in [SHARE-RELAY.md](../../development/SHARE-RELAY.md). The recipient page decrypts a stream (AES-256-GCM chunks with a counter nonce and a last-chunk flag, framing in [SHARE-RELAY.md](../../development/SHARE-RELAY.md)) without holding the whole file in memory. Decryption needs a secure origin, so a public share needs a real domain and certificate.

**Three reachability modes, one contract.** Each supplies a public HTTPS base URL; the engine stores `share.mode` and `share.base_url` and checks it.

| Mode | The person provides | Fetchpath does |
|---|---|---|
| Tunnel (Cloudflare Tunnel on the person's domain, Tailscale Funnel, any HTTPS tunnel) | A tunnel to the share server's loopback port and the resulting URL | Serve loopback only; nothing network-related |
| Relay | A VPS running `fetchpath-relay` with a domain | Engine dials out; relay obtains its own certificate and holds no files |
| Direct | Port forward and a domain (DDNS or the person's own) | Obtain a certificate (ACME); expose only the share port |

- **Relay** is stateless, has no accounts, and accepts links only from engines enrolled by its owner (an enrollment key), so it is not an open file host. Whoever runs a relay owns its cost and abuse duty; the project operates none. The default is no relay, which means T3 works only after setup.
- **Tunnel** vendors cannot read the file passively because of the fragment key (they could still serve a modified page; see §5). Their terms about large files (for example Cloudflare's CDN terms) are the person's responsibility and the guide says so.
- **Direct** exposes the home PC to the internet. The setup warns, and the Windows Firewall rule opens the share port only. A DNS API token for certificates (for example a Cloudflare token) is a secret held by the engine and is not the default path; prefer Tunnel for such domains.
- No automatic router port opening (UPnP) unless the person turns it on explicitly.

## 6. Guided setup

A "Sharing setup" flow in the desktop and terminal:

1. Plain questions: do you have a domain, a VPS, or a tunnel?
2. Exact steps for the answer, with copy-paste commands.
3. A test link and QR code. The person scans it on a phone with Wi-Fi off; a downloaded test file proves real reachability without any third-party probe.

The flow never stores secrets in logs or docs, and sharing stays off until the test passes and the person turns it on.

## 7. Agents

The MCP adapter gains `offer_file`, `list_inbox` and `accept`. An agent offer runs under D1 policy: grants, size limit and rate. Off-machine delivery or a public link always waits for the person (§3). Instructions in downloaded content are data (existing rule); a file name or page title never changes what an agent may send.

## 8. Gates

| Gate | Evidence |
|---|---|
| X1 No implicit share | Test: download, agent offer and automatic mode create no link or send without the person's action |
| X2 Provenance | Test: a credentialed or cookie-bearing origin is refused for send and link |
| X3 Grants | Test: an agent cannot offer a path outside its grants, a staged file or an unrelated file |
| X4 Ticket | Tests: single-use, expiry, revocation mid-transfer, changed file refused, Range resume |
| X5 Ports | Test: the share port serves only `/s/<id>`; a request with another listener's Host is refused |
| X6 Fragment key | Test: the key never appears in a server log, request or response header |
| X7 Reachability | Phone-on-mobile-data test per mode in the guide; each mode run on a real machine |
| X8 Review | Independent strong review of file authorization, ticket handling, encryption framing and credential/secret handling before T3 ships; SECURITY.md scope updated in the same change that adds the listener |

## 9. Backlog proposal (not applied)

- Reframe FP-093 as this design's umbrella, or keep it for S3/S4 and add a new task for T1. The user decides; nothing in the backlog changes before review.
- T1 task (bounded packet, unblocked): manifest, inbox and the three MCP tools. Needs a small contract change (new job kind, new principal permissions) as a decision in JOB-CONTRACT before code.
- T2 task: gated on the FP-091 review.
- T3 tasks, in order: Tunnel adapter plus guide, Relay, Direct with ACME. X8 review before the first ships.

## 10. Open decisions

| Id | Question | Leaning |
|---|---|---|
| T-O1 | Custom thin relay or iroh's relay with its WebAssembly browser client | Resolved (FP-116): thin custom relay. iroh needs a self-built 4.26 MB wasm client (1.39 MB gzipped) and adds a second browser path; see [SHARE-RELAY.md](../../development/SHARE-RELAY.md) |
| T-O2 | Encryption framing and library | Resolved (FP-116): STREAM-style chunked AES-256-GCM, HKDF-SHA256, WebCrypto in the page, `aes-gcm`/`hkdf` in Rust; X8 review still applies |
| T-O3 | Does an agent-to-agent inbox across users or machines exist | Not now: same machine only |
| T-O4 | Whether the person may share a credentialed download anyway (agents never may) | Allowed by explicit choice with a visible warning |
