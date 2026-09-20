# Fetchpath local fixture benchmark

This is a bounded localhost HTTP/1.1 sanity harness. It starts a deterministic Node fixture server, downloads its 1 MiB stable file with `curl --http1.1`, and records curl's version, negotiated protocol, timing, downloaded bytes, and an independently calculated SHA-256. Curl disables its user config, bypasses proxies, and has a 10-second transfer timeout.

It is **not comparative performance evidence**. Network traffic stays on `127.0.0.1`; the fixture exists to exercise downloader boundaries before there is a production downloader.

```powershell
node tools/bench/benchmark.mjs --output-dir work/bench
node tools/bench/benchmark.mjs --output-dir work/bench --repetitions 5 --size 1048576
node --test tests/bench/fixture-server.test.mjs
```

`--repetitions` is bounded to 1–20 and `--size` to 1–16 MiB. The raw artifact is `fetchpath-benchmark-raw.json` in the requested output directory. Fixture endpoints are `/files/stable` (Range and ETag), `/files/ignore-range` (intentionally 200s), `/files/truncated` (declared length exceeds body), and `/files/changed?variant=a|b` (distinct content and ETag).
