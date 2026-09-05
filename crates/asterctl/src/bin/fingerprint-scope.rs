// SPDX-License-Identifier: MIT OR Apache-2.0

use ab_glyph::PxScale;
use anyhow::Context;
use asterctl::fingerprint::{TouchEvent, parse_usb_id, start_timed_touch_listener};
use asterctl::font::FontHandler;
use asterctl_lcd::{AooScreenBuilder, DISPLAY_SIZE, ToRgb565};
use bytes::BytesMut;
use clap::Parser;
use image::{Rgb, RgbImage};
use imageproc::drawing::{draw_line_segment_mut, draw_text_mut};
use std::collections::VecDeque;
use std::thread::sleep;
use std::time::{Duration, Instant};

const PLOT_LEFT: u32 = 58;
const PLOT_RIGHT: u32 = 936;
const PLOT_TOP: u32 = 72;
const PLOT_BOTTOM: u32 = 238;
const HIGH_Y: u32 = 104;
const LOW_Y: u32 = 210;
const INFO_TOP: u32 = 258;

const BG: Rgb<u8> = Rgb([7, 12, 20]);
const GRID: Rgb<u8> = Rgb([31, 49, 64]);
const TEXT: Rgb<u8> = Rgb([205, 221, 232]);
const MUTED: Rgb<u8> = Rgb([111, 137, 153]);
const TRACE: Rgb<u8> = Rgb([52, 255, 142]);
const CURSOR: Rgb<u8> = Rgb([255, 183, 64]);
const HIT: Rgb<u8> = Rgb([60, 231, 131]);
const MISS: Rgb<u8> = Rgb([255, 106, 92]);

/// Display a cyclic, locally refreshed fingerprint-touch timing scope on the GEM12 LCD.
#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    /// LCD serial device.
    #[arg(long, default_value = "/dev/ttyACM0")]
    device: String,

    /// Fingerprint USB device ID in vid:pid notation.
    #[arg(long, default_value = "3274:8012")]
    fingerprint: String,

    /// Horizontal sample period in milliseconds.
    #[arg(long, default_value_t = 40)]
    sample_ms: u64,

    /// Minimum interval between batched LCD partial updates.
    #[arg(long, default_value_t = 125)]
    display_ms: u64,

    /// Approximate duration represented by one complete sweep.
    #[arg(long, default_value_t = 12.0)]
    sweep_seconds: f32,

    /// Maximum released gap for double-tap diagnostics.
    #[arg(long, default_value_t = 1000)]
    double_tap_ms: u64,

    /// Minimum released gap for double-tap diagnostics.
    #[arg(long, default_value_t = 150)]
    double_tap_min_ms: u64,

    /// Long-press threshold shown by the diagnostics.
    #[arg(long, default_value_t = 2000)]
    long_press_ms: u64,
}

#[derive(Default)]
struct Metrics {
    state: bool,
    pressed_at: Option<Instant>,
    last_press: Option<Instant>,
    last_release: Option<Instant>,
    last_short_release: Option<Instant>,
    second_tap: bool,
    hold: Option<Duration>,
    up_gap: Option<Duration>,
    press_period: Option<Duration>,
    release_period: Option<Duration>,
    verdict: &'static str,
    events: VecDeque<String>,
    transitions: VecDeque<(Instant, bool)>,
}

struct ScopeFrame {
    image: RgbImage,
    rgb565: BytesMut,
}

impl ScopeFrame {
    fn new(image: RgbImage) -> Self {
        let rgb565 = (&image).to_rgb565_le();
        Self { image, rgb565 }
    }

    fn sync_rect(&mut self, left: u32, top: u32, width: u32, height: u32) {
        let right = (left + width).min(DISPLAY_SIZE.0);
        let bottom = (top + height).min(DISPLAY_SIZE.1);
        for y in top..bottom {
            for x in left..right {
                let pixel = self.image.get_pixel(x, y).0;
                let value = ((pixel[0] & 248) as u16) << 8
                    | ((pixel[1] & 252) as u16) << 3
                    | u16::from(pixel[2]) >> 3;
                let offset = ((y * DISPLAY_SIZE.0 + x) * 2) as usize;
                let bytes = value.to_le_bytes();
                self.rgb565[offset] = bytes[0];
                self.rgb565[offset + 1] = bytes[1];
            }
        }
    }
}

impl ToRgb565 for &ScopeFrame {
    fn to_rgb565_le(&self) -> BytesMut {
        self.rgb565.clone()
    }
}

impl Metrics {
    fn handle(
        &mut self,
        event: TouchEvent,
        now: Instant,
        origin: Instant,
        double_tap: (Duration, Duration),
        long_press: Duration,
    ) {
        match event {
            TouchEvent::Pressed if !self.state => {
                self.state = true;
                self.transitions.push_back((now, true));
                self.pressed_at = Some(now);
                self.up_gap = self.last_release.map(|last| now.duration_since(last));
                self.press_period = self.last_press.map(|last| now.duration_since(last));
                self.last_press = Some(now);
                self.second_tap = self.last_short_release.is_some()
                    && self
                        .up_gap
                        .is_some_and(|gap| gap >= double_tap.0 && gap <= double_tap.1);
                self.last_short_release = None;
                self.verdict = if self.second_tap {
                    "SECOND TAP / RELEASE TO CONFIRM"
                } else {
                    "WAITING FOR RELEASE"
                };
                self.push_event(origin, now, "PRESSED");
            }
            TouchEvent::Released if self.state => {
                self.state = false;
                self.transitions.push_back((now, false));
                self.hold = self
                    .pressed_at
                    .take()
                    .map(|start| now.duration_since(start));
                self.release_period = self.last_release.map(|last| now.duration_since(last));
                self.last_release = Some(now);

                let short = self.hold.is_some_and(|hold| hold < long_press);
                let double = short && self.second_tap;
                self.second_tap = false;
                self.verdict = if double {
                    self.last_short_release = None;
                    "DOUBLE TAP: HIT"
                } else if short {
                    self.last_short_release = Some(now);
                    "SINGLE TAP / WAITING"
                } else {
                    self.last_short_release = None;
                    "LONG PRESS"
                };
                self.push_event(origin, now, "RELEASED");
            }
            TouchEvent::Released => {
                // The listener emits the initial idle state after connecting.
                self.state = false;
                self.transitions.push_back((now, false));
                self.push_event(origin, now, "READY / RELEASED");
            }
            TouchEvent::Pressed => {}
        }
    }

    fn push_event(&mut self, origin: Instant, now: Instant, name: &str) {
        let line = format!("{:>8.3}s  {name}", now.duration_since(origin).as_secs_f64());
        println!("{line}");
        self.events.push_front(line);
        self.events.truncate(3);
    }

    fn state_at(&self, at: Instant) -> bool {
        self.transitions
            .iter()
            .rev()
            .find_map(|(event_at, state)| (*event_at <= at).then_some(*state))
            .unwrap_or(false)
    }
}

fn main() -> anyhow::Result<()> {
    env_logger::init();
    let args = Args::parse();
    anyhow::ensure!(args.sample_ms > 0, "--sample-ms must be greater than zero");
    anyhow::ensure!(
        args.display_ms > 0,
        "--display-ms must be greater than zero"
    );
    anyhow::ensure!(args.sweep_seconds > 0.0, "--sweep-seconds must be positive");
    anyhow::ensure!(
        args.double_tap_min_ms <= args.double_tap_ms,
        "--double-tap-min-ms must not exceed --double-tap-ms"
    );

    let sample_period = Duration::from_millis(args.sample_ms);
    let display_period = Duration::from_millis(args.display_ms);
    let double_tap = (
        Duration::from_millis(args.double_tap_min_ms),
        Duration::from_millis(args.double_tap_ms),
    );
    let long_press = Duration::from_millis(args.long_press_ms);
    let desired_slots = (args.sweep_seconds * 1000.0 / args.sample_ms as f32).max(1.0);
    let step = (((PLOT_RIGHT - PLOT_LEFT) as f32 / desired_slots).round() as u32).max(1);
    let slots = (PLOT_RIGHT - PLOT_LEFT) / step;
    let actual_sweep = sample_period.mul_f32(slots as f32);

    let origin = Instant::now();
    let (vid, pid) = parse_usb_id(&args.fingerprint)?;
    let touch_events = start_timed_touch_listener(vid, pid, Duration::from_millis(20));

    let mut screen = AooScreenBuilder::new().open_device(&args.device)?;
    screen.init()?;

    let font = FontHandler::default_font();
    let base = draw_base(&font, actual_sweep, sample_period, double_tap, long_press);
    let mut frame = ScopeFrame::new(base.clone());
    screen.send_image(&frame)?;

    let mut metrics = Metrics::default();
    let mut x = PLOT_LEFT;
    let mut previous_x = x;
    let mut plotted_state = false;
    let mut next_sample = Instant::now();
    let mut next_display = Instant::now();
    let mut info_dirty = false;

    loop {
        while let Ok(event) = touch_events.try_recv() {
            metrics.handle(event.event, event.at, origin, double_tap, long_press);
            info_dirty = true;
        }

        let now = Instant::now();
        if now < next_display {
            sleep((next_display - now).min(Duration::from_millis(5)));
            continue;
        }

        let mut plot_dirty = false;
        while next_sample <= now {
            let sample_state = metrics.state_at(next_sample);
            let previous_y = if plotted_state { HIGH_Y } else { LOW_Y };
            draw_line_segment_mut(
                &mut frame.image,
                (previous_x as f32, previous_y as f32),
                (
                    (previous_x + step - 1).min(PLOT_RIGHT - 1) as f32,
                    previous_y as f32,
                ),
                TRACE,
            );
            restore_rect(
                &base,
                &mut frame.image,
                x,
                PLOT_TOP,
                step,
                PLOT_BOTTOM - PLOT_TOP + 1,
            );

            let end_x = (x + step - 1).min(PLOT_RIGHT - 1);
            let y = if sample_state { HIGH_Y } else { LOW_Y };
            draw_line_segment_mut(
                &mut frame.image,
                (x as f32, y as f32),
                (end_x as f32, y as f32),
                CURSOR,
            );
            frame.sync_rect(previous_x, previous_y, step, 1);
            frame.sync_rect(x, PLOT_TOP, step, PLOT_BOTTOM - PLOT_TOP + 1);
            plotted_state = sample_state;
            previous_x = x;
            x += step;
            if x + step > PLOT_RIGHT {
                x = PLOT_LEFT;
            }
            next_sample += sample_period;
            plot_dirty = true;
        }

        let info_changed = info_dirty;
        if info_changed {
            draw_metrics(&base, &mut frame.image, &font, &metrics);
            frame.sync_rect(0, INFO_TOP, DISPLAY_SIZE.0, DISPLAY_SIZE.1 - INFO_TOP);
            info_dirty = false;
        }
        if plot_dirty || info_changed {
            screen
                .send_image(&frame)
                .context("failed to update touch scope")?;
        }
        next_display += display_period;
        if next_display < Instant::now() {
            next_display = Instant::now();
        }
    }
}

fn draw_base(
    font: &ab_glyph::FontArc,
    sweep: Duration,
    sample: Duration,
    double_tap: (Duration, Duration),
    long_press: Duration,
) -> RgbImage {
    let mut image = RgbImage::from_pixel(DISPLAY_SIZE.0, DISPLAY_SIZE.1, BG);
    draw_text_mut(
        &mut image,
        TEXT,
        24,
        16,
        PxScale::from(28.0),
        font,
        "FINGERPRINT TOUCH SCOPE",
    );
    let subtitle = format!(
        "sweep {:.1}s  sample {}ms  double {}..{}ms  long >= {}ms",
        sweep.as_secs_f32(),
        sample.as_millis(),
        double_tap.0.as_millis(),
        double_tap.1.as_millis(),
        long_press.as_millis()
    );
    draw_text_mut(
        &mut image,
        MUTED,
        490,
        24,
        PxScale::from(15.0),
        font,
        &subtitle,
    );

    for division in 0..=10 {
        let x = PLOT_LEFT + (PLOT_RIGHT - PLOT_LEFT) * division / 10;
        draw_line_segment_mut(
            &mut image,
            (x as f32, PLOT_TOP as f32),
            (x as f32, PLOT_BOTTOM as f32),
            GRID,
        );
    }
    for y in [HIGH_Y, (HIGH_Y + LOW_Y) / 2, LOW_Y] {
        draw_line_segment_mut(
            &mut image,
            (PLOT_LEFT as f32, y as f32),
            (PLOT_RIGHT as f32, y as f32),
            GRID,
        );
    }
    draw_text_mut(
        &mut image,
        TRACE,
        8,
        (HIGH_Y - 11) as i32,
        PxScale::from(14.0),
        font,
        "DOWN",
    );
    draw_text_mut(
        &mut image,
        MUTED,
        18,
        (LOW_Y - 11) as i32,
        PxScale::from(14.0),
        font,
        "UP",
    );
    image
}

fn draw_metrics(base: &RgbImage, frame: &mut RgbImage, font: &ab_glyph::FontArc, m: &Metrics) {
    restore_rect(
        base,
        frame,
        0,
        INFO_TOP,
        DISPLAY_SIZE.0,
        DISPLAY_SIZE.1 - INFO_TOP,
    );
    let state = if m.state {
        "DOWN / PRESSED"
    } else {
        "UP / RELEASED"
    };
    let state_color = if m.state { TRACE } else { TEXT };
    draw_text_mut(
        frame,
        state_color,
        24,
        INFO_TOP as i32,
        PxScale::from(23.0),
        font,
        state,
    );

    let timing = format!(
        "hold {}   up-gap {}   press-period {}   release-period {}",
        duration_text(m.hold),
        duration_text(m.up_gap),
        duration_text(m.press_period),
        duration_text(m.release_period),
    );
    draw_text_mut(
        frame,
        TEXT,
        260,
        (INFO_TOP + 3) as i32,
        PxScale::from(17.0),
        font,
        &timing,
    );
    let verdict_color = if m.verdict.contains("HIT") {
        HIT
    } else if m.verdict == "LONG PRESS" {
        MISS
    } else {
        CURSOR
    };
    draw_text_mut(
        frame,
        verdict_color,
        24,
        (INFO_TOP + 36) as i32,
        PxScale::from(20.0),
        font,
        m.verdict,
    );
    for (index, event) in m.events.iter().enumerate() {
        draw_text_mut(
            frame,
            MUTED,
            360,
            (INFO_TOP + 35 + index as u32 * 22) as i32,
            PxScale::from(15.0),
            font,
            event,
        );
    }
}

fn duration_text(value: Option<Duration>) -> String {
    value
        .map(|duration| format!("{}ms", duration.as_millis()))
        .unwrap_or_else(|| "---".to_owned())
}

fn restore_rect(
    base: &RgbImage,
    frame: &mut RgbImage,
    left: u32,
    top: u32,
    width: u32,
    height: u32,
) {
    for y in top..(top + height).min(DISPLAY_SIZE.1) {
        for x in left..(left + width).min(DISPLAY_SIZE.0) {
            frame.put_pixel(x, y, *base.get_pixel(x, y));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_double_tap_using_up_gap() {
        let origin = Instant::now();
        let mut metrics = Metrics::default();
        let double_tap = (Duration::from_millis(150), Duration::from_secs(1));
        let long_press = Duration::from_secs(2);

        metrics.handle(TouchEvent::Pressed, origin, origin, double_tap, long_press);
        metrics.handle(
            TouchEvent::Released,
            origin + Duration::from_millis(100),
            origin,
            double_tap,
            long_press,
        );
        metrics.handle(
            TouchEvent::Pressed,
            origin + Duration::from_millis(400),
            origin,
            double_tap,
            long_press,
        );
        metrics.handle(
            TouchEvent::Released,
            origin + Duration::from_millis(1400),
            origin,
            double_tap,
            long_press,
        );

        assert_eq!(metrics.verdict, "DOUBLE TAP: HIT");
        assert_eq!(metrics.hold, Some(Duration::from_millis(1000)));
        assert_eq!(metrics.up_gap, Some(Duration::from_millis(300)));
        assert_eq!(metrics.release_period, Some(Duration::from_millis(1300)));
    }

    #[test]
    fn reports_long_press_and_resets_double_tap_candidate() {
        let origin = Instant::now();
        let mut metrics = Metrics::default();
        metrics.handle(
            TouchEvent::Pressed,
            origin,
            origin,
            (Duration::from_millis(150), Duration::from_secs(1)),
            Duration::from_secs(2),
        );
        metrics.handle(
            TouchEvent::Released,
            origin + Duration::from_secs(2),
            origin,
            (Duration::from_millis(150), Duration::from_secs(1)),
            Duration::from_secs(2),
        );

        assert_eq!(metrics.verdict, "LONG PRESS");
        assert!(metrics.last_short_release.is_none());
    }
}
