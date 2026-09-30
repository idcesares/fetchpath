# Fetchpath HTTP benchmark

Paired, seeded-random-order downloads of one generated object through the Fetchpath engine and baseline tools, on fixture profiles that the Node fixture server shapes itself, or on a small recorded internet corpus.

```powershell
cargo build --release -p fetchpath-http --bin fetchpath-http-bench -p fetchpath --bin fetchpath --locked
node tools/bench/benchmark.mjs --output-dir work/bench --size 64m --repetitions 5
node tools/bench/benchmark.mjs --output-dir work/bench --profile per-connection-limit,stall?every=3&ms=1000 --clients fetchpath-http,curl,aria2-x16
node tools/bench/benchmark.mjs --output-dir work/bench-net --corpus internet --repetitions 3
node --test tests/bench/fixture-server.test.mjs
```

Options: `--profile` (comma list of `name[?param=value&...]`, default `unshaped` plus every shaped profile), `--size` (bytes, or with k/m/g suffix, up to 1 GiB), `--repetitions` (1 to 50), `--seed`, `--clients` (`fetchpath-http`, `fetchpath-http-rounds`, `fetchpath-cli`, `fetchpath-cli-rounds`, `curl`, `aria2`, `aria2-x16`, `wget2`; default `fetchpath-http,curl`), `--timeout-s`, `--keep-files`, `--corpus fixture|internet`, `--corpus-file`.

Point it at any build with `--http-bench PATH` (or `FETCHPATH_HTTP_BENCH`), `--cli PATH` (`FETCHPATH_CLI`), `--aria2c PATH` (`ARIA2C`), `--wget2 PATH` (`WGET2`). Defaults are `target/release/fetchpath-http-bench`, `target/release/fetchpath`, and `aria2c` or `wget2` on PATH. A client whose tool is missing is skipped and the reason is recorded in the artifact and printed; it never fails the run. The `-rounds` clients run the same binaries with `FETCHPATH_HTTP_SCHEDULER=rounds`, the FP-015 round scheduler that FP-085 kept behind a switch for comparison; the CLI variants start their own engine in `<output-dir>/<client>-data` and stop it at the end. `FETCHPATH_BENCH_BUFFER_KIB` and `FETCHPATH_BENCH_MAX_LANES` (read by `fetchpath-http-bench` only) override the receive buffer and the lane cap, and `FETCHPATH_HTTP_TRACE=1` prints the scheduler's events to standard error. The artifact and the console name repository paths relative to the repository and any other tool by file name, so raw output can be committed. `fetchpath-cli` runs `fetchpath download LINK DEST --json` against an engine started with `FETCHPATH_APP_DATA_DIR` set to `<output-dir>/cli-data`, after one unmeasured warm-up download that starts the engine. aria2 runs are `aria2c` with defaults, and with `-x16 -s16 -k1M`. curl is one plain HTTP/1.1 connection that follows redirects.

## Fixture profiles

Served at `/p/<name>`; every parameter can be overridden in the query. All support `Range` and a strong ETag unless stated. Legacy unshaped paths remain: `/files/stable` (also used as profile `unshaped`), `/files/ignore-range`, `/files/truncated`, `/files/changed?variant=a|b`.

| profile | shaping | parameters (default) |
|---|---|---|
| `per-connection-limit` | one token bucket per TCP connection, so more connections give more throughput | `rate` bytes/s (8 MiB) |
| `per-client-limit` | one bucket shared by every connection from the client, so more connections do not help | `rate` (16 MiB) |
| `delay` | wait before the response headers of every request, then paced writes per connection | `ms` (50), `rate` (32 MiB, 0 = unpaced) |
| `stall` | every Nth range request of at least `minBytes` sends headers and `after` of its body, then pauses | `every` (4), `ms` (3000, 0 = until the client gives up), `after` (0.5), `minBytes` (65536) |
| `redirect` | 302 from the first listener to the file on a second listener for every request | none |
| `weak-etag` | ranges allowed, ETag is `W/"..."`, so `If-Range` never matches | none |
| `no-etag` | ranges allowed, no ETag | none |

A stall counter and all buckets reset before each measured run, so a run is deterministic given the request sequence. A single-request client such as curl never reaches the Nth request and so is not stalled. Object bytes are generated from the offset (period 256), so up to 1 GiB is served without holding it in memory; its SHA-256 is computed once, streaming, at start.

The fixture runs in a worker thread. Each request is recorded with connection id, range, status and bytes sent; the runner reports connections that carried at least one request, requests, redirects and stalled requests per run. `/probe` is served by a third listener and is never counted.

## What is measured per run

- `wallMs`: tool process start to exit. `verifyMs`: SHA-256 of the output file afterwards. `timeToVerifiedMs`: their sum. A run counts only if the exit code is 0 and the hash matches. Internet entries with no known hash record the first successful run's hash and flag it as self-recorded.
- `goodputMiBps`: output bytes over `wallMs`.
- `cpuSeconds` and `peakMemoryMiB`: user plus kernel CPU and peak working set of the directly spawned process, polled every 20 ms from a PowerShell observer on Windows. Windows CPU time has about 15.6 ms resolution, so short runs are coarse. Child processes and the separate engine process behind `fetchpath-cli` are not included. On other platforms the values are null with a recorded reason.
- A probe request every 100 ms to a tiny endpoint while the tool runs: p50, p95 and max response time. In internet mode it is a request to the Cloudflare trace endpoint.
- Per client and profile: median, min, max and a seeded percentile-bootstrap 95% interval of the median (2000 resamples, from 3 runs up). With 5 runs the interval understates the true uncertainty.

The raw JSON is `fetchpath-benchmark-raw.json` in the output directory, with a markdown table printed to stdout.

## Limits

- Everything is on `127.0.0.1`. Shaping is application level: rate, delay and stalls are emulated by the fixture, not by the network. Packet loss, congestion control, real RTT, and TCP behavior under loss cannot be emulated here and are not claimed.
- Loopback timings compare scheduling behavior between builds. They are not an Internet speed claim. Internet mode is one connection at one moment.
- The harness and the tools share one machine, so CPU-bound differences show up in timing.
- The protocol matrix (`protocol-matrix.mjs`) is separate and unchanged: it exercises HTTP/1.1 and cleartext HTTP/2 prior knowledge.

## Process interruption

After building the CLI, run `node tools/bench/interruption.mjs work/http-interruption`
with an empty scratch directory. It starts its own engine, kills that process
without a graceful checkpoint drain, then resumes a paced 256 MiB fixture and
checks the final hash, resume offset and lost-byte envelope. The JSON record
is in that scratch directory. It does not touch an installed engine, and does
not establish OS-crash or power-loss durability.

## Resumed transfer comparison

`node tools/bench/resume.mjs work/bench-resume` needs an empty scratch directory
and the release benchmark binary. It seeds the same 8 MiB prefix of a 64 MiB
fixture and runs five alternating one-lane/adaptive pairs on the per-connection
limit, checking every full-file hash. The one-lane baseline isolates the
benefit of parallel scheduling on resume; it is not a historical core build.
The binary also accepts `--resume OFFSET ETAG TOTAL PREFIX URL OUTPUT`.
