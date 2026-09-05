// SPDX-License-Identifier: MIT OR Apache-2.0

use crate::fingerprint::TouchEvent;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GestureAction {
    Wake,
    NextPanel,
    Sleep,
}

pub struct GestureController {
    long_press: Duration,
    double_tap_min: Duration,
    double_tap_max: Duration,
    pressed_at: Option<Instant>,
    last_short_release: Option<Instant>,
    second_tap: bool,
    ignore_until_release: bool,
    long_press_fired: bool,
}

impl GestureController {
    pub fn new(long_press: Duration, double_tap_min: Duration, double_tap_max: Duration) -> Self {
        Self {
            long_press,
            double_tap_min,
            double_tap_max,
            pressed_at: None,
            last_short_release: None,
            second_tap: false,
            ignore_until_release: false,
            long_press_fired: false,
        }
    }

    pub fn handle(
        &mut self,
        event: TouchEvent,
        screen_on: bool,
        now: Instant,
    ) -> Option<GestureAction> {
        match event {
            TouchEvent::Pressed => {
                if self.pressed_at.is_some() {
                    return None;
                }
                self.pressed_at = Some(now);
                self.long_press_fired = false;
                if !screen_on {
                    self.ignore_until_release = true;
                    self.last_short_release = None;
                    self.second_tap = false;
                    Some(GestureAction::Wake)
                } else {
                    self.second_tap = self.last_short_release.is_some_and(|last| {
                        let gap = now.saturating_duration_since(last);
                        gap >= self.double_tap_min && gap <= self.double_tap_max
                    });
                    self.last_short_release = None;
                    None
                }
            }
            TouchEvent::Released => {
                let pressed_at = self.pressed_at.take()?;
                if self.ignore_until_release {
                    self.ignore_until_release = false;
                    return None;
                }
                if self.long_press_fired {
                    self.long_press_fired = false;
                    self.second_tap = false;
                    return None;
                }
                if now.saturating_duration_since(pressed_at) >= self.long_press {
                    self.last_short_release = None;
                    self.second_tap = false;
                    return screen_on.then_some(GestureAction::Sleep);
                }

                if self.second_tap {
                    self.second_tap = false;
                    return screen_on.then_some(GestureAction::NextPanel);
                }
                self.last_short_release = Some(now);
                None
            }
        }
    }

    pub fn tick(&mut self, screen_on: bool, now: Instant) -> Option<GestureAction> {
        if !screen_on || self.ignore_until_release || self.long_press_fired {
            return None;
        }
        let pressed_at = self.pressed_at?;
        if now.saturating_duration_since(pressed_at) >= self.long_press {
            self.long_press_fired = true;
            self.last_short_release = None;
            self.second_tap = false;
            return Some(GestureAction::Sleep);
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn controller() -> GestureController {
        GestureController::new(
            Duration::from_secs(2),
            Duration::from_millis(150),
            Duration::from_secs(1),
        )
    }

    #[test]
    fn touch_wakes_sleeping_screen_and_ignores_release() {
        let start = Instant::now();
        let mut controller = controller();
        assert_eq!(
            controller.handle(TouchEvent::Pressed, false, start),
            Some(GestureAction::Wake)
        );
        assert_eq!(
            controller.handle(TouchEvent::Released, true, start + Duration::from_secs(3)),
            None
        );
    }

    #[test]
    fn two_short_taps_switch_panel() {
        let start = Instant::now();
        let mut controller = controller();
        assert_eq!(controller.handle(TouchEvent::Pressed, true, start), None);
        assert_eq!(
            controller.handle(
                TouchEvent::Released,
                true,
                start + Duration::from_millis(80)
            ),
            None
        );
        assert_eq!(
            controller.handle(
                TouchEvent::Pressed,
                true,
                start + Duration::from_millis(250)
            ),
            None
        );
        assert_eq!(
            controller.handle(
                TouchEvent::Released,
                true,
                start + Duration::from_millis(330)
            ),
            Some(GestureAction::NextPanel)
        );
    }

    #[test]
    fn second_press_duration_does_not_consume_double_tap_window() {
        let start = Instant::now();
        let mut controller = controller();
        controller.handle(TouchEvent::Pressed, true, start);
        controller.handle(
            TouchEvent::Released,
            true,
            start + Duration::from_millis(100),
        );
        controller.handle(
            TouchEvent::Pressed,
            true,
            start + Duration::from_millis(300),
        );
        assert_eq!(
            controller.handle(
                TouchEvent::Released,
                true,
                start + Duration::from_millis(1200)
            ),
            Some(GestureAction::NextPanel)
        );
    }

    #[test]
    fn taps_below_minimum_gap_do_not_switch_panel() {
        let start = Instant::now();
        let mut controller = controller();
        controller.handle(TouchEvent::Pressed, true, start);
        controller.handle(
            TouchEvent::Released,
            true,
            start + Duration::from_millis(100),
        );
        controller.handle(
            TouchEvent::Pressed,
            true,
            start + Duration::from_millis(200),
        );
        assert_eq!(
            controller.handle(
                TouchEvent::Released,
                true,
                start + Duration::from_millis(250)
            ),
            None
        );
    }

    #[test]
    fn slow_taps_do_not_switch_panel() {
        let start = Instant::now();
        let mut controller = controller();
        controller.handle(TouchEvent::Pressed, true, start);
        controller.handle(
            TouchEvent::Released,
            true,
            start + Duration::from_millis(80),
        );
        controller.handle(
            TouchEvent::Pressed,
            true,
            start + Duration::from_millis(1500),
        );
        assert_eq!(
            controller.handle(
                TouchEvent::Released,
                true,
                start + Duration::from_millis(1600)
            ),
            None
        );
    }

    #[test]
    fn long_press_sleeps_at_deadline() {
        let start = Instant::now();
        let mut controller = controller();
        controller.handle(TouchEvent::Pressed, true, start);
        assert_eq!(
            controller.tick(true, start + Duration::from_millis(1999)),
            None
        );
        assert_eq!(
            controller.tick(true, start + Duration::from_secs(2)),
            Some(GestureAction::Sleep)
        );
        assert_eq!(controller.tick(false, start + Duration::from_secs(3)), None);
        assert_eq!(
            controller.handle(TouchEvent::Released, false, start + Duration::from_secs(3)),
            None
        );
    }
}
