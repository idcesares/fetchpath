# FP-015 measured adaptive HTTP transfers

Date: 21 September 2026  
Acceptance: A03, A05, A06, A09

## Delivered behavior

`fetchpath-http` now owns bounded HTTP protocol selection, identity probing, adaptive range scheduling, and raw transfer diagnostics. The core feeds its existing checkpoint and publication contract from verified, ascending chunks:

- A one-byte `Range` probe establishes exact length and a strong ETag. A server that returns `200` instead streams sequentially without buffering the full object.
- Every segmented request uses `Accept-Encoding: identity`, an exact byte range, and `If-Range` with the probe's strong ETag. Status, `Content-Range`, ETag, and actual byte count must all match.
- The controller starts at one request, requires two healthy observations before growing, reduces concurrency after a measured 20% rate regression, and applies a two-observation cooldown.
- One process-wide budget caps active requests at 8 and buffered range bytes at 8 MiB. Each buffered segment reserves its full capacity before a request starts and releases it through RAII on success, failure, panic propagation, or cancellation.
- Chunks are sorted and passed to the core only in ascending order. Adaptive protocol/range failure resets the unpublished staging file before the established sequential path runs; sink, cancellation, checkpoint, and publication errors do not take that fallback.
- The static libcurl build now includes nghttp2. Automatic HTTPS requests prefer H2 and may negotiate lower; controlled cleartext H2 uses an explicit prior-knowledge mode. The packaged build still reports HTTP/3 unavailable, so the policy records `http3_unavailable` and does not claim or simulate an H3 attempt.

## Verification

The Rust suites cover strict range parsing, controller hysteresis/cooldown, impossible reservations, concurrent global budget enforcement, the existing interruption/publication adversarial cases, and a 5 MiB multi-request reassembly through the production core. The Node fixture suite retains Range, validator, refusal, source-change, and truncation coverage.

The controlled protocol matrix produced matching 5 MiB hashes over both H1 and H2-prior-knowledge. H1 recorded preferred H2, negotiated HTTP/1.1, and explicit H3/lower-protocol fallback. H2 recorded negotiated `h2`; H3 was not attempted because the packaged capability is false. Raw observations are in [the protocol matrix](evidence/http-adaptive/fp015-protocol-matrix.json).

The paired pilot used five deterministically randomized 8 MiB loopback pairs. Every Fetchpath and curl output matched SHA-256 `a9407298d138c39f01e2067ad330ea65db7fa553a7cb541fd8bfd243c5405c45`. Fetchpath's wall-clock median was 81.3978 ms and curl's was 56.8628 ms. Fetchpath was therefore slower in this pilot; the first Fetchpath sample also showed a large startup outlier. Peak observed Fetchpath concurrency was 2, peak active requests was 2, and peak buffered bytes was 2,097,152, all within the configured global limits. See [the raw paired observations](evidence/http-adaptive/fp015-loopback-pilot.json).

## Limits carried forward

No speed advantage is claimed. Loopback does not model RTT, loss, congestion, blocked UDP, slow storage, or competing traffic, and five pairs cannot establish tail percentiles. The current worker design uses bounded independent easy handles; it negotiates H2 but does not yet share one multiplexed H2 connection across ranges. A packaged HTTP/3 backend, controlled H3 endpoint, and blocked-UDP fallback run remain required before any H3 support claim.
