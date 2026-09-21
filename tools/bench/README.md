# Fetchpath adaptive HTTP benchmark

This harness runs paired, deterministically randomized downloads of the same generated object through the production `fetchpath-http` engine and curl's HTTP/1.1 baseline. It records wall-clock time to a flushed file, independently verifies each SHA-256, and retains negotiated-protocol, range adaptation, concurrency, and global request/buffer budget observations.

```powershell
node tools/bench/benchmark.mjs --output-dir work/bench
node tools/bench/benchmark.mjs --output-dir work/bench --repetitions 5 --size 8388608 --seed 15015
node tools/bench/protocol-matrix.mjs work/protocol-matrix
node --test tests/bench/fixture-server.test.mjs
```

`--repetitions` is bounded to 1–20 and `--size` to 1–16 MiB. The raw artifact is `fetchpath-benchmark-raw.json` in the requested output directory. Output files are intentionally retained so their recorded hashes can be independently checked.

This is correctness and instrumentation evidence on `127.0.0.1`, not an Internet speed claim. It does not model RTT, loss, UDP blocking, slow storage, competing traffic, or enough repetitions for tail percentiles. The fixture endpoints remain `/files/stable` (Range and ETag), `/files/ignore-range` (intentionally 200s), `/files/truncated` (declared length exceeds body), and `/files/changed?variant=a|b` (distinct content and ETag).

The protocol matrix adds controlled HTTP/1.1 and cleartext HTTP/2-prior-knowledge endpoints. HTTP/3 is not attempted when the packaged libcurl reports it unavailable; the recorded fallback is capability-driven. This does not substitute for a future packaged HTTP/3 endpoint and blocked-UDP run.
