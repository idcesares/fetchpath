//! Smoothed per-download transfer rate and ETA estimation.

/// A smoothed view of how fast one download is actually moving.
///
/// Progress is sampled whenever the interface asks for the queue, which is an
/// irregular interval, so the rate is computed from the elapsed time between
/// samples rather than assuming a fixed cadence. Samples closer together than
/// `MIN_INTERVAL_MS` are ignored: dividing a handful of bytes by a few
/// milliseconds produces a number that swings wildly and reads as noise.
#[derive(Clone, Copy, Default)]
pub(crate) struct RateEstimate {
    last_sample_ms: u64,
    last_bytes: u64,
    started: bool,
    smoothed_bytes_per_second: Option<f64>,
}

impl RateEstimate {
    const MIN_INTERVAL_MS: u64 = 400;
    /// Weight given to the newest sample. Low enough that one slow scheduling
    /// hiccup does not make the displayed rate jump.
    const SMOOTHING: f64 = 0.3;
    /// After this long with no progress the rate is no longer describing
    /// anything that is happening, so it is withdrawn rather than left frozen.
    const STALE_AFTER_MS: u64 = 5_000;
    const FIRST_ESTIMATE_BYTES: u64 = 64 * 1024;

    pub(crate) fn observe(&mut self, bytes: u64, now_ms: u64) {
        if !self.started || bytes < self.last_bytes {
            // First sample, or the transfer restarted from a lower offset.
            // There is no interval to measure yet, so record a baseline only.
            *self = Self {
                last_sample_ms: now_ms,
                last_bytes: bytes,
                started: true,
                smoothed_bytes_per_second: None,
            };
            return;
        }
        let elapsed_ms = now_ms.saturating_sub(self.last_sample_ms);
        if elapsed_ms < Self::MIN_INTERVAL_MS {
            return;
        }
        let delta = bytes - self.last_bytes;
        // The first estimate waits for the transfer proper: a transfer opens
        // with a one-byte range probe, and 1 byte over a second read as
        // "1 B/s, thousands of hours left" (FP-074). The baseline stays put,
        // so a genuinely slow link is averaged from its start.
        if self.smoothed_bytes_per_second.is_none()
            && delta < Self::FIRST_ESTIMATE_BYTES
            && elapsed_ms < Self::STALE_AFTER_MS
        {
            return;
        }
        if delta == 0 && elapsed_ms >= Self::STALE_AFTER_MS {
            self.smoothed_bytes_per_second = None;
            self.last_sample_ms = now_ms;
            return;
        }
        let instant = delta as f64 * 1_000.0 / elapsed_ms as f64;
        self.smoothed_bytes_per_second = Some(match self.smoothed_bytes_per_second {
            Some(previous) => Self::SMOOTHING * instant + (1.0 - Self::SMOOTHING) * previous,
            None => instant,
        });
        self.last_sample_ms = now_ms;
        self.last_bytes = bytes;
    }

    /// Clears the estimate when a download stops, so a paused or finished row
    /// never shows a speed it is not achieving.
    pub(crate) fn clear(&mut self) {
        *self = Self::default();
    }

    pub(crate) fn bytes_per_second(&self) -> Option<u64> {
        self.smoothed_bytes_per_second
            .filter(|rate| *rate >= 1.0)
            .map(|rate| rate as u64)
    }

    /// Remaining time, only when a total and a real rate are both known.
    pub(crate) fn eta_seconds(&self, received: u64, total: Option<u64>) -> Option<u64> {
        let total = total?;
        let rate = self.bytes_per_second()?;
        let remaining = total.checked_sub(received)?;
        (remaining > 0).then(|| remaining.div_ceil(rate))
    }
}
