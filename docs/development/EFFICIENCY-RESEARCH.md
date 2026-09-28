# Efficiency research: chunking, deltas, paths and coding

FP-023 and FP-024, 28 September 2026. Question: would content-defined
chunking, deltas, several network interfaces or forward error correction
save a Fetchpath user time or bytes, net of their costs? Decision: **none is
built now.** One narrow candidate remains, below.

## Chunking and deltas between related versions (FP-023)

Four pairs of consecutive public releases, measured with
`tools/bench/cdc-compare.rs` (FastCDC, 16/64/256 KiB, ~910 MB/s here) and
`tools/bench/delta-compare.py` (zstd 19 with the old version as a raw
dictionary, as `--patch-from`). [evidence](evidence/research/fp023-cdc-delta.json)

| New version | Size | Held in old as chunks | zstd delta | New compressed alone |
| --- | --- | --- | --- | --- |
| `sqlite3.c` 3.53.4 (source) | 9.5 MB | 81% | 2 KB | 1.76 MB |
| `yt-dlp.exe` 2026.08.19 | 17.8 MB | 49% | 3.9 MB | 17.5 MB |
| `ffmpeg.exe` 9.0.2 (rebuilt binary) | 105 MB | 2.7% | 24.2 MB | 30.5 MB |
| ffmpeg 9.0.2 zip (compressed archive) | 115 MB | 1.1% | 113.9 MB | 114.0 MB |

What it means for a downloader:

- **Compressed archives, which most software ships as, share almost
  nothing.** A rebuilt binary shares little as chunks; zstd finds more, but
  only 20% less than compressing the new version alone.
- **Neither works without the publisher.** To fetch only the missing chunks,
  a client needs the new version's chunk list with a trusted identity (a
  "reconstruction mapping"); to apply a delta, someone must build and publish
  it against the version the client holds. Arbitrary websites offer neither,
  and a client that invents them is back to downloading the whole file. The
  mapping must itself be trusted: every chunk checked by its hash and the
  whole file by a digest the publisher states, as the cache already does.
- **Cost is small when it applies**: chunking at ~0.9 GB/s, a 40-byte list
  entry per 64 KiB chunk (under 0.05% of the file).

**Candidate kept:** Hugging Face's Xet storage already publishes exactly such
reconstruction metadata for model files, and the official client uses it
(`hf_xet`). A Xet client in `adapters/providers` would let a new model
version reuse chunks from the cache. It is worth measuring on real model
version pairs before building; Fetchpath uses the HTTP bridge today
([providers](PROVIDERS.md)).

## Several interfaces, and coding (FP-024)

This machine has one physical uplink (1 Gbps Ethernet; the Tailscale adapter
rides the same link), so aggregation could not be measured, and no claim is
made either way. Standards as checked on 28 September 2026:

- Windows 11 has no Multipath TCP for applications; Linux has it since 5.6
  and iOS for clients ([mptcp.dev FAQ](https://www.mptcp.dev/faq.html)).
- Multipath QUIC is an Internet-Draft awaiting IESG approval
  ([draft-ietf-quic-multipath](https://datatracker.ietf.org/doc/draft-ietf-quic-multipath)),
  and the packaged libcurl has no HTTP/3 at all.
- The one application-level option is ranges over two interfaces, each
  socket bound to one (`CURLOPT_INTERFACE`). It needs two usable uplinks,
  often one of them metered (a phone's hotspot), so cost and battery belong
  to the person, and it takes a server's capacity twice over: a fairness
  question the plan already treats as a gate.
- Forward error correction helps one-way or multicast delivery with no
  retransmission. A download over TCP or QUIC is already repaired by the
  transport; application FEC would add bytes to every transfer and save
  none. Not pursued.

The bottleneck actually measured this session was Fetchpath's own HTTP path
(new connection per range, quadratic checkpoint hashing), now fixed: see
[adaptive HTTP](ADAPTIVE-HTTP.md). That, not the transport, was where time
went.

**Next action, only if wanted:** a two-uplink measurement (Ethernet plus a
phone hotspot) of per-interface ranges against a single link, with the
person choosing whether a metered link may be used at all.
