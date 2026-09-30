//! The concurrency controller (spec 4.6). It decides how many lanes a
//! download runs and never looks at bytes or sockets: the scheduler feeds it
//! the goodput of each valid window.
//!
//! Start at 2 lanes. Measure one valid window, then try double the width. Keep
//! it only if two consecutive valid windows at the new width each beat the old
//! width by 15%; otherwise go back and never probe again for this download.
//! Halve on a 429 or 503, on backpressure that lasts two windows, or on a 30%
//! goodput drop that lasts two windows.

/// A window at a wider setting must beat the old one by this factor.
pub(crate) const GROWTH_GAIN: f64 = 1.15;
/// A window below this share of the reference counts as a drop.
pub(crate) const DROP_RATIO: f64 = 0.70;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Change {
    Grew(usize),
    Reverted(usize),
    Halved(usize),
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Stage {
    /// Measuring the starting width.
    Baseline,
    /// Trying a wider setting against `baseline`.
    Probe {
        from: usize,
        baseline: f64,
        good_windows: u8,
        good_sum: f64,
    },
    /// No further probing.
    Settled,
}

#[derive(Debug)]
pub(crate) struct Controller {
    width: usize,
    max: usize,
    stage: Stage,
    reference: Option<f64>,
    drops: u8,
    backpressure_windows: u8,
}

/// What a window told the controller.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct Outcome {
    pub(crate) change: Option<Change>,
    /// Stop claiming new work for the rest of this window.
    pub(crate) hold_claims: bool,
}

impl Controller {
    pub(crate) fn new(start: usize, max: usize) -> Self {
        let max = max.max(1);
        Self {
            width: start.clamp(1, max),
            max,
            stage: Stage::Baseline,
            reference: None,
            drops: 0,
            backpressure_windows: 0,
        }
    }

    pub(crate) fn width(&self) -> usize {
        self.width
    }

    /// True while the download may still ask for more lanes.
    pub(crate) fn growing(&self) -> bool {
        match self.stage {
            Stage::Baseline => self.width < self.max,
            Stage::Probe { .. } => true,
            Stage::Settled => false,
        }
    }

    /// Lowers the ceiling (the budget refused a lane, or the source ignores
    /// ranges). The width follows it down.
    pub(crate) fn cap(&mut self, max: usize) {
        self.max = max.clamp(1, self.max);
        if self.width > self.max {
            self.width = self.max;
        }
        if matches!(self.stage, Stage::Probe { .. }) && self.width >= self.max {
            // The probe can no longer reach the width it wanted.
            self.stage = Stage::Settled;
        }
    }

    /// A 429 or 503: halve and stop probing.
    pub(crate) fn throttled(&mut self) -> usize {
        self.halve();
        self.width
    }

    fn halve(&mut self) {
        self.width = (self.width / 2).max(1);
        self.stage = Stage::Settled;
        self.reference = None;
        self.drops = 0;
        self.backpressure_windows = 0;
    }

    /// Feeds one valid window.
    pub(crate) fn window(&mut self, goodput: f64, backpressure: bool) -> Outcome {
        let mut outcome = Outcome::default();
        if backpressure {
            self.backpressure_windows += 1;
            if let Stage::Probe { from, baseline, .. } = self.stage {
                self.width = from;
                self.stage = Stage::Settled;
                self.reference = Some(baseline);
                outcome.change = Some(Change::Reverted(from));
            } else if self.backpressure_windows >= 2 {
                self.halve();
                outcome.change = Some(Change::Halved(self.width));
            } else {
                outcome.hold_claims = true;
            }
            return outcome;
        }
        self.backpressure_windows = 0;
        match self.stage {
            Stage::Baseline => {
                self.reference = Some(goodput);
                if self.width < self.max {
                    self.stage = Stage::Probe {
                        from: self.width,
                        baseline: goodput,
                        good_windows: 0,
                        good_sum: 0.0,
                    };
                    self.width = (self.width * 2).min(self.max);
                    outcome.change = Some(Change::Grew(self.width));
                } else {
                    self.stage = Stage::Settled;
                }
            }
            Stage::Probe {
                from,
                baseline,
                good_windows,
                good_sum,
            } => {
                if goodput >= baseline * GROWTH_GAIN {
                    let good_windows = good_windows + 1;
                    let good_sum = good_sum + goodput;
                    if good_windows >= 2 {
                        let mean = good_sum / f64::from(good_windows);
                        self.reference = Some(mean);
                        if self.width < self.max {
                            self.stage = Stage::Probe {
                                from: self.width,
                                baseline: mean,
                                good_windows: 0,
                                good_sum: 0.0,
                            };
                            self.width = (self.width * 2).min(self.max);
                            outcome.change = Some(Change::Grew(self.width));
                        } else {
                            self.stage = Stage::Settled;
                        }
                    } else {
                        self.stage = Stage::Probe {
                            from,
                            baseline,
                            good_windows,
                            good_sum,
                        };
                    }
                } else {
                    self.width = from;
                    self.stage = Stage::Settled;
                    self.reference = Some(baseline);
                    outcome.change = Some(Change::Reverted(from));
                }
            }
            Stage::Settled => match self.reference {
                Some(reference) if goodput < reference * DROP_RATIO => {
                    self.drops += 1;
                    if self.drops >= 2 {
                        self.halve();
                        outcome.change = Some(Change::Halved(self.width));
                    }
                }
                Some(_) => self.drops = 0,
                None => self.reference = Some(goodput),
            },
        }
        outcome
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_at_two_lanes_and_doubles_only_after_two_windows_that_gain_15_percent() {
        let mut controller = Controller::new(2, 8);
        assert_eq!(controller.width(), 2);
        // The starting width is measured, then 4 is tried.
        assert_eq!(
            controller.window(100.0, false).change,
            Some(Change::Grew(4))
        );
        assert_eq!(controller.window(120.0, false).change, None);
        assert_eq!(controller.width(), 4);
        // The second good window keeps 4 and tries 8.
        assert_eq!(
            controller.window(125.0, false).change,
            Some(Change::Grew(8))
        );
        assert_eq!(controller.window(150.0, false).change, None);
        assert_eq!(controller.window(160.0, false).change, None);
        assert_eq!(controller.width(), 8);
        assert!(!controller.growing());
    }

    #[test]
    fn a_probe_that_does_not_gain_goes_back_and_never_probes_again() {
        let mut controller = Controller::new(2, 8);
        controller.window(100.0, false);
        // 10% is not enough.
        assert_eq!(
            controller.window(110.0, false).change,
            Some(Change::Reverted(2))
        );
        assert_eq!(controller.width(), 2);
        assert!(!controller.growing());
        for _ in 0..6 {
            assert_eq!(controller.window(100.0, false).change, None);
        }
        assert_eq!(controller.width(), 2, "no re-probe");
    }

    #[test]
    fn one_weak_window_out_of_two_reverts() {
        let mut controller = Controller::new(2, 4);
        controller.window(100.0, false);
        assert_eq!(controller.window(130.0, false).change, None);
        assert_eq!(
            controller.window(105.0, false).change,
            Some(Change::Reverted(2))
        );
    }

    #[test]
    fn a_throttle_halves_and_stops_probing() {
        let mut controller = Controller::new(2, 8);
        controller.window(100.0, false);
        controller.window(130.0, false);
        controller.window(135.0, false);
        assert_eq!(controller.width(), 8);
        assert_eq!(controller.throttled(), 4);
        assert_eq!(controller.throttled(), 2);
        assert_eq!(controller.throttled(), 1);
        assert_eq!(controller.throttled(), 1);
        assert!(!controller.growing());
    }

    #[test]
    fn a_sustained_thirty_percent_drop_halves() {
        let mut controller = Controller::new(4, 4);
        controller.window(100.0, false);
        assert_eq!(controller.window(65.0, false).change, None);
        assert_eq!(
            controller.window(60.0, false).change,
            Some(Change::Halved(2))
        );
        // A single dip between good windows does nothing.
        let mut controller = Controller::new(4, 4);
        controller.window(100.0, false);
        controller.window(60.0, false);
        controller.window(95.0, false);
        assert_eq!(controller.window(60.0, false).change, None);
        assert_eq!(controller.width(), 4);
    }

    #[test]
    fn backpressure_holds_claims_once_then_halves() {
        let mut controller = Controller::new(4, 4);
        controller.window(100.0, false);
        let first = controller.window(100.0, true);
        assert!(first.hold_claims);
        assert_eq!(first.change, None);
        assert_eq!(
            controller.window(100.0, true).change,
            Some(Change::Halved(2))
        );
    }

    #[test]
    fn backpressure_during_a_probe_reverts_it() {
        let mut controller = Controller::new(2, 8);
        controller.window(100.0, false);
        assert_eq!(
            controller.window(200.0, true).change,
            Some(Change::Reverted(2))
        );
    }

    #[test]
    fn a_ceiling_from_the_budget_pulls_the_width_down() {
        let mut controller = Controller::new(2, 8);
        controller.window(100.0, false);
        assert_eq!(controller.width(), 4);
        controller.cap(3);
        assert_eq!(controller.width(), 3);
        assert!(!controller.growing());
    }

    #[test]
    fn one_lane_never_probes() {
        let mut controller = Controller::new(2, 1);
        assert_eq!(controller.width(), 1);
        assert_eq!(controller.window(100.0, false).change, None);
        assert!(!controller.growing());
    }
}
