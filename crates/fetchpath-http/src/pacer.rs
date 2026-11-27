//! Hierarchical token-bucket bandwidth pacing for HTTP transfers (FP-150).
//!
//! Each download can attach a [`BandwidthPacer`] combining an engine-wide
//! global [`TokenBucket`] with an optional per-job [`TokenBucket`]. Rates are
//! stored atomically (`0` = unlimited) so live updates from `UpdateSettings`
//! or `UpdatePolicy` take effect immediately on running transfers without
//! restarting them.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Lowest non-zero bandwidth limit the pacer enforces (16 KiB/s).
pub const MIN_BANDWIDTH_BYTES_PER_SECOND: u64 = 16 * 1024;

/// Maximum sleep slice before re-checking cancellation and live rate changes.
const SLEEP_SLICE: Duration = Duration::from_millis(20);

/// Deficits shorter than this wait threshold are accumulated rather than
/// sleeping, preventing coarse OS timer ticks (e.g. Windows 15.6 ms) from
/// over-sleeping on tiny sub-millisecond remainders.
const MIN_SLEEP_THRESHOLD: Duration = Duration::from_millis(8);

#[derive(Debug)]
struct BucketState {
    tokens: f64,
    last: Instant,
}

/// Thread-safe, lock-light token bucket with atomic live rate updates.
///
/// Reservations deduct from `tokens` under a short mutex hold and sleep
/// *outside* the lock, allowing multiple concurrent jobs to share a global
/// bucket fairly without head-of-line blocking or starvation.
#[derive(Debug)]
pub struct TokenBucket {
    rate_bps: AtomicU64,
    state: Mutex<BucketState>,
}

impl TokenBucket {
    /// Creates a token bucket with `rate_bytes_per_second` (`0` = unlimited).
    pub fn new(rate_bytes_per_second: u64) -> Self {
        Self {
            rate_bps: AtomicU64::new(normalize_rate(rate_bytes_per_second)),
            state: Mutex::new(BucketState {
                tokens: 0.0,
                last: Instant::now(),
            }),
        }
    }

    /// Returns the currently configured ceiling in bytes/second (`0` = unlimited).
    pub fn rate(&self) -> u64 {
        self.rate_bps.load(Ordering::Relaxed)
    }

    /// Updates the ceiling live (`0` = unlimited). Clamps accumulated debt to
    /// the new burst window so raising or clearing a limit takes effect at once.
    pub fn set_rate(&self, rate_bytes_per_second: u64) {
        let normalized = normalize_rate(rate_bytes_per_second);
        self.rate_bps.store(normalized, Ordering::Relaxed);
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.last = Instant::now();
        if normalized == 0 {
            state.tokens = 0.0;
        } else {
            let burst = burst_capacity(normalized);
            state.tokens = state.tokens.clamp(-burst, burst);
        }
    }

    /// Reserves `bytes` against this bucket and returns the required delay
    /// alongside the rate snapshot used to compute it.
    fn reserve(&self, bytes: usize) -> (Duration, u64) {
        let rate = self.rate_bps.load(Ordering::Relaxed);
        if rate == 0 || bytes == 0 {
            return (Duration::ZERO, 0);
        }
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        let elapsed = now.saturating_duration_since(state.last).as_secs_f64();
        state.last = now;
        let burst = burst_capacity(rate);
        state.tokens = (state.tokens + elapsed * rate as f64).min(burst);
        state.tokens -= bytes as f64;
        if state.tokens >= 0.0 {
            return (Duration::ZERO, rate);
        }
        let wait_secs = (-state.tokens) / rate as f64;
        let wait = Duration::from_secs_f64(wait_secs.min(60.0));
        if wait < MIN_SLEEP_THRESHOLD {
            (Duration::ZERO, rate)
        } else {
            (wait, rate)
        }
    }
}

/// Hierarchical bandwidth pacer enforcing both a shared global ceiling and an
/// optional per-job ceiling.
///
/// The effective delay for a chunk is `max(global_delay, job_delay)`, so a
/// job can never exceed the global ceiling regardless of its own limit, while
/// a single job whose per-job limit equals the global limit is never double-
/// charged.
#[derive(Clone, Debug)]
pub struct BandwidthPacer {
    global: Arc<TokenBucket>,
    job: Option<Arc<TokenBucket>>,
}

impl BandwidthPacer {
    pub fn new(global: Arc<TokenBucket>, job: Option<Arc<TokenBucket>>) -> Self {
        Self { global, job }
    }

    /// Creates a pacer backed by a single bucket (useful for tests or standalone
    /// callers).
    pub fn single(bucket: Arc<TokenBucket>) -> Self {
        Self {
            global: bucket,
            job: None,
        }
    }

    /// Returns `(global_bps, job_bps)` for redacted diagnostic output.
    pub fn configured_rates(&self) -> (u64, u64) {
        (
            self.global.rate(),
            self.job.as_ref().map_or(0, |bucket| bucket.rate()),
        )
    }

    /// Accounts for `bytes` just received and sleeps as needed while checking
    /// `cancelled` and live rate increases/removals.
    pub fn consume(&self, bytes: usize, cancelled: &dyn Fn() -> bool) {
        if bytes == 0 {
            return;
        }
        let (global_wait, global_rate) = self.global.reserve(bytes);
        let (job_wait, job_rate) = self
            .job
            .as_ref()
            .map_or((Duration::ZERO, 0), |bucket| bucket.reserve(bytes));

        let wait = global_wait.max(job_wait);
        if wait.is_zero() {
            return;
        }

        let started = Instant::now();
        let mut deadline = started + wait;
        loop {
            if cancelled() {
                return;
            }
            // If the controlling bucket was raised or removed live, shorten or
            // end the sleep immediately rather than stalling on a stale limit.
            let adjusted =
                self.adjusted_wait(started, bytes, global_wait, global_rate, job_wait, job_rate);
            let target = started + adjusted;
            if target < deadline {
                deadline = target;
            }
            let now = Instant::now();
            let Some(remaining) = deadline.checked_duration_since(now) else {
                return;
            };
            if remaining.is_zero() {
                return;
            }
            std::thread::sleep(remaining.min(SLEEP_SLICE));
        }
    }

    fn adjusted_wait(
        &self,
        _started: Instant,
        bytes: usize,
        global_wait: Duration,
        global_rate: u64,
        job_wait: Duration,
        job_rate: u64,
    ) -> Duration {
        let g = scale_wait(global_wait, global_rate, self.global.rate(), bytes);
        let j = match self.job.as_ref() {
            Some(bucket) => scale_wait(job_wait, job_rate, bucket.rate(), bytes),
            None => Duration::ZERO,
        };
        g.max(j)
    }
}

fn scale_wait(
    initial_wait: Duration,
    initial_rate: u64,
    current_rate: u64,
    bytes: usize,
) -> Duration {
    if initial_wait.is_zero() || initial_rate == 0 {
        return Duration::ZERO;
    }
    if current_rate == 0 {
        return Duration::ZERO;
    }
    if current_rate <= initial_rate {
        return initial_wait;
    }
    let ratio = initial_rate as f64 / current_rate as f64;
    let scaled = Duration::from_secs_f64((initial_wait.as_secs_f64() * ratio).min(60.0));
    let chunk_time = Duration::from_secs_f64((bytes as f64 / current_rate as f64).min(60.0));
    scaled.max(chunk_time)
}

fn normalize_rate(rate_bps: u64) -> u64 {
    if rate_bps == 0 {
        0
    } else {
        rate_bps.max(MIN_BANDWIDTH_BYTES_PER_SECOND)
    }
}

/// Burst headroom equal to 50 ms of the configured rate, clamped between 4 KiB
/// and 256 KiB. Small enough to keep 5 s average rates well within ±5% while
/// large enough to absorb OS sleep-timer jitter without under-running.
fn burst_capacity(rate_bps: u64) -> f64 {
    (rate_bps as f64 * 0.05).clamp(4_096.0, 262_144.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    #[test]
    fn unlimited_bucket_never_delays() {
        let pacer = BandwidthPacer::single(Arc::new(TokenBucket::new(0)));
        let started = Instant::now();
        for _ in 0..100 {
            pacer.consume(64 * 1024, &|| false);
        }
        assert!(started.elapsed() < Duration::from_millis(50));
    }

    #[test]
    fn cancellation_interrupts_pacing_promptly() {
        let bucket = Arc::new(TokenBucket::new(MIN_BANDWIDTH_BYTES_PER_SECOND));
        let pacer = BandwidthPacer::single(bucket);
        let cancelled = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&cancelled);
        let trigger = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(40));
            flag.store(true, Ordering::Relaxed);
        });
        let started = Instant::now();
        // 160 KiB at 16 KiB/s would take 10 s without cancellation.
        pacer.consume(160 * 1024, &|| cancelled.load(Ordering::Relaxed));
        trigger.join().unwrap();
        assert!(
            started.elapsed() < Duration::from_millis(300),
            "expected prompt cancel wakeup, took {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn raising_rate_to_unlimited_wakes_sleeping_consumer() {
        let bucket = Arc::new(TokenBucket::new(MIN_BANDWIDTH_BYTES_PER_SECOND));
        let pacer = BandwidthPacer::single(Arc::clone(&bucket));
        let updater = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(40));
            bucket.set_rate(0);
        });
        let started = Instant::now();
        pacer.consume(160 * 1024, &|| false);
        updater.join().unwrap();
        assert!(
            started.elapsed() < Duration::from_millis(300),
            "expected prompt rate-lift wakeup, took {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn hierarchical_pacer_does_not_double_charge_equal_limits() {
        let rate = 256 * 1024;
        let global = Arc::new(TokenBucket::new(rate));
        let job = Arc::new(TokenBucket::new(rate));
        let pacer = BandwidthPacer::new(global, Some(job));
        let started = Instant::now();
        let total = 256 * 1024;
        let chunk = 16 * 1024;
        for _ in 0..(total / chunk) {
            pacer.consume(chunk, &|| false);
        }
        let elapsed = started.elapsed().as_secs_f64();
        let observed = total as f64 / elapsed;
        let ratio = observed / rate as f64;
        assert!(
            (0.85..=1.15).contains(&ratio),
            "expected ~{rate} B/s, got {observed:.0} B/s (ratio {ratio:.3}, elapsed {elapsed:.3}s)"
        );
    }
}
