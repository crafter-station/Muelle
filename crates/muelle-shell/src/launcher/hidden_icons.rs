use std::time::{Duration, Instant};

pub(super) const OPEN_DELAY: Duration = Duration::from_millis(250);
pub(super) const HOLD: Duration = Duration::from_millis(1200);

#[derive(Default)]
enum Phase {
    #[default]
    Idle,
    Opening(Instant),
    Waiting(Instant),
    Open(Instant),
    Closing {
        until: Instant,
        dismissed: bool,
        reopen: bool,
    },
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Action {
    FocusTray,
    Activate,
    Dismiss,
    Reposition,
    None,
}

/// One controller owns both the delayed Enter and the flyout's animation.
/// Cancelling an opening therefore cannot leave a stale worker behind.
#[derive(Default)]
pub(super) struct Toggle {
    phase: Phase,
}

impl Toggle {
    pub(super) fn active(&self) -> bool {
        !matches!(self.phase, Phase::Idle)
    }

    pub(super) fn click(&mut self, now: Instant, visible: bool) -> Action {
        if let Phase::Closing { reopen, .. } = &mut self.phase {
            // Explorer may still be processing Escape. Preserve toggle intent
            // until dismissal completes instead of sending another Escape.
            *reopen = !*reopen;
            return Action::None;
        }
        if visible || matches!(self.phase, Phase::Opening(_) | Phase::Waiting(_)) {
            self.phase = if visible || matches!(self.phase, Phase::Waiting(_)) {
                Phase::Closing {
                    until: now + HOLD,
                    dismissed: visible,
                    reopen: false,
                }
            } else {
                // Enter has not been sent, so nothing can open later.
                Phase::Idle
            };
            Action::Dismiss
        } else {
            self.phase = Phase::Opening(now + OPEN_DELAY);
            Action::FocusTray
        }
    }

    pub(super) fn tick(&mut self, now: Instant, visible: bool) -> Action {
        match self.phase {
            Phase::Opening(_) if visible => {
                self.phase = Phase::Open(now + HOLD);
                Action::Reposition
            }
            Phase::Opening(at) if now >= at => {
                self.phase = Phase::Waiting(now + HOLD);
                Action::Activate
            }
            Phase::Waiting(until) | Phase::Open(until) if now >= until => {
                self.phase = Phase::Idle;
                Action::None
            }
            Phase::Waiting(until) if visible => {
                self.phase = Phase::Open(until);
                Action::Reposition
            }
            Phase::Open(_) if !visible => {
                self.phase = Phase::Idle;
                Action::None
            }
            Phase::Open(_) => Action::Reposition,
            Phase::Closing {
                until,
                dismissed,
                reopen,
            } if (dismissed && !visible) || now >= until => {
                if !reopen {
                    self.phase = Phase::Idle;
                    Action::None
                } else if visible {
                    self.phase = Phase::Open(now + HOLD);
                    Action::Reposition
                } else {
                    self.phase = Phase::Opening(now + OPEN_DELAY);
                    Action::FocusTray
                }
            }
            // Enter may already be queued in Explorer when the second click
            // arrives. Dismiss a late flyout once, without queuing Escapes
            // while Explorer processes the first one.
            Phase::Closing {
                until,
                dismissed: false,
                reopen,
            } if visible => {
                self.phase = Phase::Closing {
                    until,
                    dismissed: true,
                    reopen,
                };
                Action::Dismiss
            }
            _ => Action::None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_click_cancels_enter_before_the_opening_delay() {
        let now = Instant::now();
        let mut toggle = Toggle::default();
        assert_eq!(toggle.click(now, false), Action::FocusTray);
        assert_eq!(toggle.click(now + OPEN_DELAY / 2, false), Action::Dismiss);
        assert_eq!(toggle.tick(now + OPEN_DELAY, false), Action::None);
    }

    #[test]
    fn cancellation_dismisses_a_flyout_that_appears_after_enter() {
        let now = Instant::now();
        let mut toggle = Toggle::default();
        toggle.click(now, false);
        assert_eq!(toggle.tick(now + OPEN_DELAY, false), Action::Activate);
        assert_eq!(toggle.click(now + OPEN_DELAY, false), Action::Dismiss);
        assert_eq!(toggle.tick(now + OPEN_DELAY, true), Action::Dismiss);
    }

    #[test]
    fn third_click_can_open_again_after_cancellation() {
        let now = Instant::now();
        let mut toggle = Toggle::default();
        toggle.click(now, false);
        toggle.click(now, false);
        assert_eq!(toggle.click(now, false), Action::FocusTray);
        assert_eq!(toggle.tick(now + OPEN_DELAY, false), Action::Activate);
        assert_eq!(toggle.tick(now + OPEN_DELAY, true), Action::Reposition);
    }

    #[test]
    fn third_click_reopens_after_pending_dismissal() {
        let now = Instant::now();
        let mut toggle = Toggle::default();
        toggle.click(now, false);
        toggle.tick(now + OPEN_DELAY, false);
        toggle.tick(now + OPEN_DELAY, true);
        assert_eq!(toggle.click(now + OPEN_DELAY, true), Action::Dismiss);
        assert_eq!(toggle.click(now + OPEN_DELAY, true), Action::None);
        assert_eq!(toggle.tick(now + OPEN_DELAY, true), Action::None);
        assert_eq!(toggle.tick(now + OPEN_DELAY, false), Action::FocusTray);
        assert_eq!(toggle.tick(now + OPEN_DELAY * 2, false), Action::Activate);
        assert_eq!(toggle.tick(now + OPEN_DELAY * 2, true), Action::Reposition);
    }

    #[test]
    fn fourth_click_cancels_reopen_during_pending_dismissal() {
        let now = Instant::now();
        let mut toggle = Toggle::default();
        toggle.click(now, true);
        toggle.click(now, true);
        assert_eq!(toggle.click(now, true), Action::None);
        assert_eq!(toggle.tick(now, false), Action::None);
        assert!(!toggle.active());
        assert_eq!(toggle.tick(now + OPEN_DELAY, false), Action::None);
    }

    #[test]
    fn click_after_disappearance_reopens_before_the_next_tick() {
        let now = Instant::now();
        let mut toggle = Toggle::default();
        toggle.click(now, true);
        assert_eq!(toggle.click(now, false), Action::None);
        assert_eq!(toggle.tick(now, false), Action::FocusTray);
        assert_eq!(toggle.tick(now + OPEN_DELAY, false), Action::Activate);
    }

    #[test]
    fn reopen_waits_for_a_late_flyout_to_be_dismissed() {
        let now = Instant::now();
        let mut toggle = Toggle::default();
        toggle.click(now, false);
        toggle.tick(now + OPEN_DELAY, false);
        toggle.click(now + OPEN_DELAY, false);
        assert_eq!(toggle.click(now + OPEN_DELAY, false), Action::None);
        assert_eq!(toggle.tick(now + OPEN_DELAY, false), Action::None);
        assert_eq!(toggle.tick(now + OPEN_DELAY, true), Action::Dismiss);
        assert_eq!(toggle.tick(now + OPEN_DELAY, true), Action::None);
        assert_eq!(toggle.tick(now + OPEN_DELAY, false), Action::FocusTray);
        assert_eq!(toggle.tick(now + OPEN_DELAY * 2, false), Action::Activate);
    }

    #[test]
    fn reopen_recovers_when_the_cancelled_flyout_never_appears() {
        let now = Instant::now();
        let mut toggle = Toggle::default();
        toggle.click(now, false);
        toggle.tick(now + OPEN_DELAY, false);
        toggle.click(now + OPEN_DELAY, false);
        toggle.click(now + OPEN_DELAY, false);
        assert_eq!(
            toggle.tick(now + OPEN_DELAY + HOLD, false),
            Action::FocusTray
        );
        assert_eq!(
            toggle.tick(now + OPEN_DELAY * 2 + HOLD, false),
            Action::Activate
        );
    }

    #[test]
    fn second_click_closes_a_visible_flyout_during_animation() {
        let now = Instant::now();
        let mut toggle = Toggle::default();
        toggle.click(now, false);
        toggle.tick(now + OPEN_DELAY, false);
        toggle.tick(now + OPEN_DELAY, true);
        assert_eq!(toggle.click(now + OPEN_DELAY, true), Action::Dismiss);
        assert_eq!(toggle.tick(now + OPEN_DELAY, true), Action::None);
    }

    #[test]
    fn closing_stops_observing_after_the_flyout_disappears() {
        let now = Instant::now();
        let mut toggle = Toggle::default();
        toggle.click(now, true);
        toggle.tick(now, false);
        assert!(!toggle.active());
        assert_eq!(toggle.tick(now, true), Action::None);
    }

    #[test]
    fn flyout_already_visible_before_enter_is_repositioned_without_activation() {
        let now = Instant::now();
        let mut toggle = Toggle::default();
        toggle.click(now, false);
        assert_eq!(toggle.tick(now + OPEN_DELAY, true), Action::Reposition);
    }

    #[test]
    fn visible_flyout_closes_even_after_repositioning_stops() {
        let now = Instant::now();
        let mut toggle = Toggle::default();
        toggle.click(now, false);
        toggle.tick(now + OPEN_DELAY, false);
        toggle.tick(now + OPEN_DELAY, true);
        toggle.tick(now + OPEN_DELAY + HOLD, true);
        assert!(!toggle.active());
        assert_eq!(toggle.click(now + OPEN_DELAY + HOLD, true), Action::Dismiss);
    }

    #[test]
    fn outside_dismissal_allows_the_next_click_to_open() {
        let now = Instant::now();
        let mut toggle = Toggle::default();
        toggle.click(now, false);
        toggle.tick(now + OPEN_DELAY, false);
        toggle.tick(now + OPEN_DELAY, true);
        toggle.tick(now + OPEN_DELAY, false);
        assert_eq!(toggle.click(now + OPEN_DELAY, false), Action::FocusTray);
    }

    #[test]
    fn missing_flyout_eventually_returns_to_idle() {
        let now = Instant::now();
        let mut toggle = Toggle::default();
        toggle.click(now, false);
        toggle.tick(now + OPEN_DELAY, false);
        toggle.tick(now + OPEN_DELAY + HOLD, false);
        assert!(!toggle.active());
        assert_eq!(
            toggle.click(now + OPEN_DELAY + HOLD, false),
            Action::FocusTray
        );
    }
}
