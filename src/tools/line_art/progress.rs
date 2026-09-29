//! The worker's progress protocol and the live runtime estimate built on it.
//!
//! The worker prints one flat JSON object per line on stdout (see
//! `line_art_worker.py`). Lines that do not parse as a known event are
//! ignored, so library output that reaches stdout cannot break a run.
use std::{
    iter::Peekable,
    str::Chars,
    time::{Duration, Instant},
};

use super::estimate::{
    self, Calibration, EXIT_SECONDS, IMAGES, LOAD_SECONDS, STARTUP_SECONDS, WARMUP_SECONDS,
};

/// Reference encoding and decoding of one image, relative to its denoising
/// steps: about 2.4% for the 9B model at 512².
const IMAGE_OVERHEAD_SHARE: f64 = 0.03;
/// The bar stays short of complete until the worker reports completion.
const MAX_RUNNING_FRACTION: f64 = 0.99;
/// Bounds of the correction the first measured step applies to the model.
const FIRST_STEP_RATIO: (f64, f64) = (0.25, 4.);

/// How far a running generation is, for a progress bar.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Progress {
    /// 0–1, never decreasing within one run.
    pub fraction: f64,
    /// Estimated time until the worker finishes.
    pub remaining: Duration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum WorkerImage {
    LineArt,
    Fill,
}

impl WorkerImage {
    fn index(self) -> u32 {
        match self {
            Self::LineArt => 0,
            Self::Fill => 1,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(super) enum WorkerEvent {
    /// The text encoder encodes `prompts` prompts without cached embeddings.
    Encode {
        prompts: u32,
    },
    Load,
    /// Denoising step `step` of `steps` finished `elapsed` seconds after
    /// `image`'s generation started.
    Step {
        image: WorkerImage,
        step: u32,
        steps: u32,
        elapsed: f64,
    },
    Decode {
        image: WorkerImage,
    },
    Done,
}

impl WorkerEvent {
    pub(super) fn parse(line: &str) -> Option<Self> {
        let fields = parse_flat_object(line)?;
        let field = |name: &str| {
            fields
                .iter()
                .find_map(|(key, value)| (key == name).then_some(value))
        };
        let text = |name| match field(name)? {
            Value::Text(text) => Some(text.as_str()),
            Value::Number(_) => None,
        };
        let number = |name| match field(name)? {
            Value::Number(number) => Some(*number),
            Value::Text(_) => None,
        };
        let count = |name| {
            let number = number(name)?;
            (number.fract() == 0. && (0. ..=f64::from(u32::MAX)).contains(&number))
                .then_some(number as u32)
        };
        let image = || match text("image")? {
            "line_art" => Some(WorkerImage::LineArt),
            "fill" => Some(WorkerImage::Fill),
            _ => None,
        };
        match text("event")? {
            "stage" => match text("stage")? {
                "encode" => Some(Self::Encode {
                    prompts: count("prompts")?,
                }),
                "load" => Some(Self::Load),
                "decode" => Some(Self::Decode { image: image()? }),
                _ => None,
            },
            "step" => {
                let (step, steps, elapsed) = (count("step")?, count("steps")?, number("elapsed")?);
                (1..=steps).contains(&step).then_some(())?;
                (elapsed >= 0.).then_some(())?;
                Some(Self::Step {
                    image: image()?,
                    step,
                    steps,
                    elapsed,
                })
            }
            "done" => Some(Self::Done),
            _ => None,
        }
    }
}

#[derive(Debug, PartialEq)]
enum Value {
    Text(String),
    Number(f64),
}

/// A JSON object whose values are strings or finite numbers, alone on the
/// line. Nothing else is needed for the worker protocol.
fn parse_flat_object(line: &str) -> Option<Vec<(String, Value)>> {
    let mut chars = line.trim().chars().peekable();
    (chars.next()? == '{').then_some(())?;
    let mut fields = Vec::new();
    skip_whitespace(&mut chars);
    if chars.peek() == Some(&'}') {
        chars.next();
    } else {
        loop {
            skip_whitespace(&mut chars);
            (chars.next()? == '"').then_some(())?;
            let key = parse_string(&mut chars)?;
            skip_whitespace(&mut chars);
            (chars.next()? == ':').then_some(())?;
            skip_whitespace(&mut chars);
            let value = if chars.peek() == Some(&'"') {
                chars.next();
                Value::Text(parse_string(&mut chars)?)
            } else {
                Value::Number(parse_number(&mut chars)?)
            };
            fields.push((key, value));
            skip_whitespace(&mut chars);
            match chars.next()? {
                ',' => {}
                '}' => break,
                _ => return None,
            }
        }
    }
    chars.next().is_none().then_some(fields)
}

fn skip_whitespace(chars: &mut Peekable<Chars>) {
    while chars.peek().is_some_and(|c| c.is_ascii_whitespace()) {
        chars.next();
    }
}

/// The rest of a string whose opening quote was consumed.
fn parse_string(chars: &mut Peekable<Chars>) -> Option<String> {
    let mut text = String::new();
    loop {
        match chars.next()? {
            '"' => return Some(text),
            '\\' => text.push(match chars.next()? {
                '"' => '"',
                '\\' => '\\',
                '/' => '/',
                'b' => '\u{8}',
                'f' => '\u{c}',
                'n' => '\n',
                'r' => '\r',
                't' => '\t',
                'u' => {
                    let hex = (0..4).map(|_| chars.next()).collect::<Option<String>>()?;
                    char::from_u32(u32::from_str_radix(&hex, 16).ok()?)?
                }
                _ => return None,
            }),
            c if c.is_control() => return None,
            c => text.push(c),
        }
    }
}

fn parse_number(chars: &mut Peekable<Chars>) -> Option<f64> {
    let mut number = String::new();
    while let Some(&c) = chars.peek() {
        if !(c.is_ascii_digit() || matches!(c, '-' | '+' | '.' | 'e' | 'E')) {
            break;
        }
        number.push(c);
        chars.next();
    }
    number
        .parse::<f64>()
        .ok()
        .filter(|number| number.is_finite())
}

/// Turns worker events into a remaining-time estimate. Before denoising it
/// uses the calibrated model in [`estimate`]; from the first denoising step
/// on it extrapolates the measured step time.
pub(super) struct Tracker {
    size: (u32, u32),
    steps: u32,
    calibration: f64,
    started: Instant,
    /// Remaining seconds as estimated at `anchor`, the latest event.
    anchor: Instant,
    remaining_at_anchor: f64,
    /// `elapsed` of each image's first step.
    first_step: [Option<f64>; IMAGES as usize],
    step_seconds: Option<f64>,
    prompts_encoded: u32,
    fraction: f64,
    done: bool,
}

impl Tracker {
    /// `prompts_to_encode` prompts have no cached embeddings yet.
    pub(super) fn new(
        size: (u32, u32),
        steps: u32,
        prompts_to_encode: u32,
        calibration: Calibration,
        started: Instant,
    ) -> Self {
        let calibration = calibration.factor();
        Self {
            size,
            steps: steps.max(1),
            calibration,
            started,
            anchor: started,
            remaining_at_anchor: calibration
                * estimate::estimate(size, prompts_to_encode).as_secs_f64(),
            first_step: [None; IMAGES as usize],
            step_seconds: None,
            prompts_encoded: prompts_to_encode,
            fraction: 0.,
            done: false,
        }
    }

    /// Prompts encoded in this run; part of the prediction a finished run is
    /// compared against.
    pub(super) fn prompts_encoded(&self) -> u32 {
        self.prompts_encoded
    }

    /// Calibrated seconds from the model load to the end of the run.
    fn predicted_after_load(&self) -> f64 {
        self.calibration
            * (LOAD_SECONDS
                + WARMUP_SECONDS
                + f64::from(IMAGES) * estimate::image_seconds(self.size)
                + EXIT_SECONDS)
    }

    /// Calibrated seconds of one denoising step.
    fn predicted_step(&self) -> f64 {
        self.calibration * estimate::image_seconds(self.size)
            / (f64::from(self.steps) * (1. + IMAGE_OVERHEAD_SHARE))
    }

    /// Seconds after `step` of `image` finished: its remaining steps and
    /// decoding, then every later image.
    fn remaining_after(&self, image: WorkerImage, step: u32, step_seconds: f64) -> f64 {
        let steps = f64::from(self.steps);
        let later_images = f64::from(IMAGES - 1 - image.index());
        f64::from(self.steps.saturating_sub(step)) * step_seconds
            + steps * step_seconds * IMAGE_OVERHEAD_SHARE
            + later_images * steps * step_seconds * (1. + IMAGE_OVERHEAD_SHARE)
            + self.calibration * EXIT_SECONDS
    }

    pub(super) fn observe(&mut self, event: &WorkerEvent, now: Instant) {
        let remaining = match *event {
            WorkerEvent::Encode { prompts } => {
                self.prompts_encoded = prompts;
                // The generation process starts after the encoding one.
                self.calibration * (estimate::encode_seconds(prompts) + STARTUP_SECONDS)
                    + self.predicted_after_load()
            }
            WorkerEvent::Load => self.predicted_after_load(),
            WorkerEvent::Step {
                image,
                step,
                steps,
                elapsed,
            } => {
                self.steps = steps;
                let slot = &mut self.first_step[image.index() as usize];
                let step_seconds = match *slot {
                    Some(first) if step > 1 => (elapsed - first).max(0.) / f64::from(step - 1),
                    _ => {
                        *slot = Some(elapsed);
                        // The first step also encodes the reference and, in
                        // the first image, warms up; scale the model by it.
                        let warmup = if image.index() == 0 {
                            self.calibration * WARMUP_SECONDS
                        } else {
                            0.
                        };
                        let predicted = self.predicted_step();
                        let ratio = (elapsed / (predicted + warmup))
                            .clamp(FIRST_STEP_RATIO.0, FIRST_STEP_RATIO.1);
                        predicted * ratio
                    }
                };
                self.step_seconds = Some(step_seconds);
                self.remaining_after(image, step, step_seconds)
            }
            WorkerEvent::Decode { image } => {
                let step_seconds = self.step_seconds.unwrap_or_else(|| self.predicted_step());
                self.remaining_after(image, self.steps, step_seconds)
            }
            WorkerEvent::Done => {
                self.done = true;
                0.
            }
        };
        self.anchor = now;
        self.remaining_at_anchor = remaining.max(0.);
    }

    pub(super) fn progress(&mut self, now: Instant) -> Progress {
        let since_anchor = now.saturating_duration_since(self.anchor).as_secs_f64();
        let remaining = (self.remaining_at_anchor - since_anchor).max(0.);
        let elapsed = now.saturating_duration_since(self.started).as_secs_f64();
        let fraction = if self.done {
            1.
        } else if elapsed + remaining <= 0. {
            0.
        } else {
            (elapsed / (elapsed + remaining)).min(MAX_RUNNING_FRACTION)
        };
        self.fraction = self.fraction.max(fraction);
        Progress {
            fraction: self.fraction,
            remaining: Duration::from_secs_f64(remaining),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(image: WorkerImage, step: u32, elapsed: f64) -> WorkerEvent {
        WorkerEvent::Step {
            image,
            step,
            steps: 4,
            elapsed,
        }
    }

    #[test]
    fn worker_events_parse_from_json_lines() {
        use WorkerImage::{Fill, LineArt};
        for (line, event) in [
            (
                r#"{"event": "stage", "stage": "encode", "prompts": 2}"#,
                WorkerEvent::Encode { prompts: 2 },
            ),
            (r#"{"event":"stage","stage":"load"}"#, WorkerEvent::Load),
            (
                r#" {"event": "step", "image": "line_art", "step": 2, "steps": 4, "elapsed": 2.864} "#,
                step(LineArt, 2, 2.864),
            ),
            (
                r#"{"elapsed": 1e-3, "steps": 4, "step": 1, "image": "fill", "event": "step"}"#,
                step(Fill, 1, 0.001),
            ),
            (
                r#"{"event": "stage", "stage": "decode", "image": "fill"}"#,
                WorkerEvent::Decode { image: Fill },
            ),
            (
                r#"{"event": "done", "note": "café \"ok\""}"#,
                WorkerEvent::Done,
            ),
        ] {
            assert_eq!(WorkerEvent::parse(line), Some(event), "{line}");
        }
    }

    #[test]
    fn malformed_or_unknown_lines_are_ignored() {
        for line in [
            "",
            "Loading pipeline components...: 100%",
            "{",
            "{}",
            r#"{"event": "done""#,
            r#"{"event": "done"} trailing"#,
            r#"{"event": "done",}"#,
            r#"{"event": done}"#,
            r#"{"event": "unknown"}"#,
            r#"{"event": "stage", "stage": "warp"}"#,
            r#"{"event": "stage", "stage": "encode"}"#,
            r#"{"event": "stage", "stage": "encode", "prompts": 1.5}"#,
            r#"{"event": "stage", "stage": "encode", "prompts": -1}"#,
            r#"{"event": "stage", "stage": "decode", "image": "sketch"}"#,
            r#"{"event": "step", "image": "fill", "step": 5, "steps": 4, "elapsed": 1}"#,
            r#"{"event": "step", "image": "fill", "step": 0, "steps": 4, "elapsed": 1}"#,
            r#"{"event": "step", "image": "fill", "step": 1, "steps": 4, "elapsed": -1}"#,
            r#"{"event": "step", "image": "fill", "step": 1, "steps": 4, "elapsed": "1"}"#,
            r#"{"event": "step", "image": "fill", "step": 1, "steps": 4, "elapsed": 1e999}"#,
            r#"{"event": "step", "image": "fill", "step": 1, "steps": 4}"#,
            r#"{"event": ["step"]}"#,
            r#"{"event": "stage", "stage": {"name": "load"}}"#,
            r#"{"event": "bad\escape"}"#,
            "[\"done\"]",
        ] {
            assert_eq!(WorkerEvent::parse(line), None, "{line}");
        }
    }

    /// A 512² generation measured with the real 9B model on the reference
    /// machine: event times since launch, then the process exit. The third
    /// steps are interpolated.
    #[test]
    fn the_estimate_refines_from_measured_steps_and_never_goes_backwards() {
        use WorkerImage::{Fill, LineArt};
        let started = Instant::now();
        let at = |seconds: f64| started + Duration::from_secs_f64(seconds);
        let mut tracker = Tracker::new((512, 512), 4, 0, Calibration::default(), started);
        let initial = tracker.progress(started);
        assert_eq!(initial.fraction, 0.);
        assert_eq!(initial.remaining, estimate::estimate((512, 512), 0));
        let exit = 47.07;
        let timeline = [
            (4.16, WorkerEvent::Load),
            (12.18, step(LineArt, 1, 6.126)),
            (17.02, step(LineArt, 2, 10.966)),
            (21.73, step(LineArt, 3, 15.677)),
            (26.44, step(LineArt, 4, 20.388)),
            (26.44, WorkerEvent::Decode { image: LineArt }),
            (32.21, step(Fill, 1, 4.983)),
            (36.95, step(Fill, 2, 9.715)),
            (41.70, step(Fill, 3, 14.467)),
            (46.45, step(Fill, 4, 19.219)),
            (46.45, WorkerEvent::Decode { image: Fill }),
            (46.92, WorkerEvent::Done),
        ];
        let mut previous = initial.fraction;
        for (seconds, event) in &timeline {
            // Between events, the remaining time counts down.
            let before = tracker.progress(at(seconds - 0.05));
            assert!(before.fraction >= previous);
            tracker.observe(event, at(*seconds));
            let progress = tracker.progress(at(*seconds));
            assert!(progress.fraction >= before.fraction, "{event:?}");
            let actual = exit - seconds;
            let error = (progress.remaining.as_secs_f64() - actual).abs();
            if matches!(event, WorkerEvent::Step { step: 2.., .. }) {
                assert!(error < 1.5, "{event:?}: {progress:?}, actual {actual:.2} s");
            }
            // The model and the first-step correction stay within 10%.
            assert!(
                error < 0.1 * actual + 1.,
                "{event:?}: {progress:?}, actual {actual:.2} s"
            );
            previous = progress.fraction;
        }
        assert_eq!(tracker.progress(at(exit)).fraction, 1.);
        assert_eq!(tracker.progress(at(exit)).remaining, Duration::ZERO);
    }

    #[test]
    fn a_needed_encoding_is_in_the_first_estimate_and_its_stage() {
        let started = Instant::now();
        let mut tracker = Tracker::new((512, 512), 4, 2, Calibration::default(), started);
        assert_eq!(tracker.prompts_encoded(), 2);
        assert_eq!(
            tracker.progress(started).remaining,
            estimate::estimate((512, 512), 2)
        );
        // The encoding process reports its stage after its own start-up;
        // what remains includes the generation process's start-up.
        let at = started + Duration::from_secs_f64(STARTUP_SECONDS);
        tracker.observe(&WorkerEvent::Encode { prompts: 2 }, at);
        let remaining = tracker.progress(at).remaining.as_secs_f64();
        let expected = estimate::estimate((512, 512), 2).as_secs_f64() - STARTUP_SECONDS;
        assert!(
            (remaining - expected).abs() < 1e-6,
            "{remaining} vs {expected}"
        );
    }

    #[test]
    fn running_progress_stays_below_complete_and_counts_down_to_zero() {
        let started = Instant::now();
        let mut tracker = Tracker::new((160, 160), 4, 0, Calibration::default(), started);
        let late = tracker.progress(started + Duration::from_secs(600));
        assert_eq!(late.remaining, Duration::ZERO);
        assert_eq!(late.fraction, MAX_RUNNING_FRACTION);
        // A later, larger estimate cannot move the bar backwards.
        tracker.observe(
            &WorkerEvent::Encode { prompts: 2 },
            started + Duration::from_secs(601),
        );
        let encoding = tracker.progress(started + Duration::from_secs(601));
        assert_eq!(encoding.fraction, MAX_RUNNING_FRACTION);
        assert!(encoding.remaining > Duration::from_secs(10));
        assert_eq!(tracker.prompts_encoded(), 2);
    }

    #[test]
    fn a_calibrated_machine_scales_the_prediction_until_steps_are_measured() {
        let started = Instant::now();
        let slow = Calibration::default().updated(Duration::from_secs(20), Duration::from_secs(10));
        let mut tracker = Tracker::new((512, 512), 4, 0, slow, started);
        let expected = slow.factor() * estimate::estimate((512, 512), 0).as_secs_f64();
        assert!((tracker.progress(started).remaining.as_secs_f64() - expected).abs() < 1e-6);
        // Measured steps replace the calibrated model.
        let at = |seconds: f64| started + Duration::from_secs_f64(seconds);
        tracker.observe(&step(WorkerImage::LineArt, 1, 2.9), at(6.6));
        tracker.observe(&step(WorkerImage::LineArt, 2, 4.6), at(8.3));
        let measured = tracker.remaining_after(WorkerImage::LineArt, 2, 1.7);
        assert!((tracker.progress(at(8.3)).remaining.as_secs_f64() - measured).abs() < 1e-6);
    }
}
