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

use super::estimate::{self, CUTOUT_SECONDS, Calibration, IMAGES, LOAD_SECONDS, WARMUP_SECONDS};

/// Reference encoding and decoding of one image, relative to its denoising
/// steps: about 2.4% for the 9B model at 512².
const IMAGE_OVERHEAD_SHARE: f64 = 0.03;
/// The bar stays short of complete until the generation finishes.
const MAX_RUNNING_FRACTION: f64 = 0.99;
/// Bounds of the correction the first measured step applies to the model.
const FIRST_STEP_RATIO: (f64, f64) = (0.25, 4.);

/// How far a running generation is, for a progress bar.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Progress {
    /// 0–1, never decreasing within one run.
    pub fraction: f64,
    /// Estimated time until the generation finishes.
    pub remaining: Duration,
    pub stage: Stage,
}

/// What a running generation is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// FLUX generates the line art and the fill.
    Generating,
    /// BiRefNet cuts the generated fill out.
    CuttingOut,
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
    /// The resident worker loads the model, then is ready for jobs.
    Load,
    Ready,
    /// Denoising step `step` of `steps` of `job` finished `elapsed` seconds
    /// after `image`'s generation started.
    Step {
        job: u32,
        image: WorkerImage,
        step: u32,
        steps: u32,
        elapsed: f64,
    },
    Decode {
        job: u32,
        image: WorkerImage,
    },
    /// The end of `job`: its outputs are saved, it failed, or it stopped
    /// because it was cancelled.
    Done {
        job: u32,
    },
    Failed {
        job: u32,
        message: String,
    },
    Cancelled {
        job: u32,
    },
}

impl WorkerEvent {
    /// The job this event belongs to, if any.
    pub(super) fn job(&self) -> Option<u32> {
        match self {
            Self::Encode { .. } | Self::Load | Self::Ready => None,
            Self::Step { job, .. }
            | Self::Decode { job, .. }
            | Self::Done { job }
            | Self::Failed { job, .. }
            | Self::Cancelled { job } => Some(*job),
        }
    }
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
                "decode" => Some(Self::Decode {
                    job: count("job")?,
                    image: image()?,
                }),
                _ => None,
            },
            "ready" => Some(Self::Ready),
            "step" => {
                let (step, steps, elapsed) = (count("step")?, count("steps")?, number("elapsed")?);
                (1..=steps).contains(&step).then_some(())?;
                (elapsed >= 0.).then_some(())?;
                Some(Self::Step {
                    job: count("job")?,
                    image: image()?,
                    step,
                    steps,
                    elapsed,
                })
            }
            "done" => Some(Self::Done { job: count("job")? }),
            "error" => Some(Self::Failed {
                job: count("job")?,
                message: text("message")?.to_owned(),
            }),
            "cancelled" => Some(Self::Cancelled { job: count("job")? }),
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
    /// Seconds until the resident worker is ready, as predicted at the start.
    start_up: f64,
    /// Whether the worker has not generated at this size yet.
    warm_up: bool,
    /// The uncalibrated prediction of the whole run.
    predicted: Duration,
    started: Instant,
    /// Remaining seconds as estimated at `anchor`, the latest event.
    anchor: Instant,
    remaining_at_anchor: f64,
    /// `elapsed` of each image's first step.
    first_step: [Option<f64>; IMAGES as usize],
    step_seconds: Option<f64>,
    prompts_encoded: u32,
    fraction: f64,
    stage: Stage,
    done: bool,
}

impl Tracker {
    /// `prompts_to_encode` prompts have no cached embeddings yet, the
    /// resident worker needs `start_up` seconds until it is ready, and
    /// `warm_up` if it has not generated at this size yet.
    pub(super) fn new(
        size: (u32, u32),
        steps: u32,
        prompts_to_encode: u32,
        (start_up, warm_up): (f64, bool),
        calibration: Calibration,
        started: Instant,
    ) -> Self {
        let calibration = calibration.factor();
        let predicted = estimate::estimate(size, prompts_to_encode, start_up, warm_up);
        Self {
            size,
            steps: steps.max(1),
            calibration,
            start_up,
            warm_up,
            predicted,
            started,
            anchor: started,
            remaining_at_anchor: calibration * predicted.as_secs_f64(),
            first_step: [None; IMAGES as usize],
            step_seconds: None,
            prompts_encoded: prompts_to_encode,
            fraction: 0.,
            stage: Stage::Generating,
            done: false,
        }
    }

    /// Prompts encoded in this run.
    #[cfg(test)]
    pub(super) fn prompts_encoded(&self) -> u32 {
        self.prompts_encoded
    }

    /// The uncalibrated prediction a finished run is compared against.
    pub(super) fn predicted(&self) -> Duration {
        self.predicted
    }

    /// Calibrated seconds from the worker being ready to the end of the
    /// generation: the job, then the cutout.
    fn predicted_job(&self) -> f64 {
        let warm_up = if self.warm_up { WARMUP_SECONDS } else { 0. };
        self.calibration
            * (warm_up + f64::from(IMAGES) * estimate::image_seconds(self.size) + CUTOUT_SECONDS)
    }

    /// Calibrated seconds of the cutout.
    fn predicted_cutout(&self) -> f64 {
        self.calibration * CUTOUT_SECONDS
    }

    /// Calibrated seconds of one denoising step.
    fn predicted_step(&self) -> f64 {
        self.calibration * estimate::image_seconds(self.size)
            / (f64::from(self.steps) * (1. + IMAGE_OVERHEAD_SHARE))
    }

    /// Seconds after `step` of `image` finished: its remaining steps and
    /// decoding, every later image, then the cutout.
    fn remaining_after(&self, image: WorkerImage, step: u32, step_seconds: f64) -> f64 {
        let steps = f64::from(self.steps);
        let later_images = f64::from(IMAGES - 1 - image.index());
        f64::from(self.steps.saturating_sub(step)) * step_seconds
            + steps * step_seconds * IMAGE_OVERHEAD_SHARE
            + later_images * steps * step_seconds * (1. + IMAGE_OVERHEAD_SHARE)
            + self.predicted_cutout()
    }

    pub(super) fn observe(&mut self, event: &WorkerEvent, now: Instant) {
        let remaining = match *event {
            WorkerEvent::Encode { prompts } => {
                self.prompts_encoded = prompts;
                // The resident worker starts after the encoding process.
                self.calibration * (estimate::encode_seconds(prompts) + self.start_up)
                    + self.predicted_job()
            }
            WorkerEvent::Load => self.calibration * LOAD_SECONDS + self.predicted_job(),
            WorkerEvent::Ready => self.predicted_job(),
            WorkerEvent::Step {
                image,
                step,
                steps,
                elapsed,
                ..
            } => {
                self.steps = steps;
                let slot = &mut self.first_step[image.index() as usize];
                let step_seconds = match *slot {
                    Some(first) if step > 1 => (elapsed - first).max(0.) / f64::from(step - 1),
                    _ => {
                        *slot = Some(elapsed);
                        // The first step also encodes the reference and, in
                        // the first image, warms up; scale the model by it.
                        let warmup = if image.index() == 0 && self.warm_up {
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
            WorkerEvent::Decode { image, .. } => {
                let step_seconds = self.step_seconds.unwrap_or_else(|| self.predicted_step());
                self.remaining_after(image, self.steps, step_seconds)
            }
            // The job is done; the cutout follows.
            WorkerEvent::Done { .. } => self.predicted_cutout(),
            // The job ends with an error; the estimate no longer matters.
            WorkerEvent::Failed { .. } | WorkerEvent::Cancelled { .. } => 0.,
        };
        self.anchor = now;
        self.remaining_at_anchor = remaining.max(0.);
    }

    /// The job's images are saved and checked; BiRefNet cuts the fill out.
    pub(super) fn cutting_out(&mut self, now: Instant) {
        self.stage = Stage::CuttingOut;
        self.anchor = now;
        self.remaining_at_anchor = self.predicted_cutout();
    }

    /// The generation is complete.
    pub(super) fn finish(&mut self, now: Instant) {
        self.done = true;
        self.anchor = now;
        self.remaining_at_anchor = 0.;
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
            stage: self.stage,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::line_art::estimate::STARTUP_SECONDS;

    fn step(image: WorkerImage, step: u32, elapsed: f64) -> WorkerEvent {
        WorkerEvent::Step {
            job: 1,
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
            (r#"{"event": "ready"}"#, WorkerEvent::Ready),
            (
                r#" {"event": "step", "job": 1, "image": "line_art", "step": 2, "steps": 4, "elapsed": 2.864} "#,
                step(LineArt, 2, 2.864),
            ),
            (
                r#"{"elapsed": 1e-3, "steps": 4, "step": 1, "image": "fill", "job": 1, "event": "step"}"#,
                step(Fill, 1, 0.001),
            ),
            (
                r#"{"event": "stage", "stage": "decode", "job": 3, "image": "fill"}"#,
                WorkerEvent::Decode {
                    job: 3,
                    image: Fill,
                },
            ),
            (
                r#"{"event": "done", "job": 7, "note": "caf\u00e9 \"ok\""}"#,
                WorkerEvent::Done { job: 7 },
            ),
            (
                r#"{"event": "error", "job": 2, "message": "ValueError: bad \"size\""}"#,
                WorkerEvent::Failed {
                    job: 2,
                    message: "ValueError: bad \"size\"".into(),
                },
            ),
            (
                r#"{"event": "cancelled", "job": 4}"#,
                WorkerEvent::Cancelled { job: 4 },
            ),
        ] {
            assert_eq!(WorkerEvent::parse(line), Some(event), "{line}");
        }
        assert_eq!(WorkerEvent::Ready.job(), None);
        assert_eq!(step(Fill, 1, 0.).job(), Some(1));
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
            r#"{"event": "stage", "stage": "decode", "job": 1, "image": "sketch"}"#,
            r#"{"event": "step", "job": 1, "image": "fill", "step": 5, "steps": 4, "elapsed": 1}"#,
            r#"{"event": "step", "job": 1, "image": "fill", "step": 0, "steps": 4, "elapsed": 1}"#,
            r#"{"event": "step", "job": 1, "image": "fill", "step": 1, "steps": 4, "elapsed": -1}"#,
            r#"{"event": "step", "job": 1, "image": "fill", "step": 1, "steps": 4, "elapsed": "1"}"#,
            r#"{"event": "step", "job": 1, "image": "fill", "step": 1, "steps": 4, "elapsed": 1e999}"#,
            r#"{"event": "step", "job": 1, "image": "fill", "step": 1, "steps": 4}"#,
            r#"{"event": ["step"]}"#,
            r#"{"event": "stage", "stage": {"name": "load"}}"#,
            r#"{"event": "bad\escape"}"#,
            "[\"done\"]",
            r#"{"event": "done"}"#,
            r#"{"event": "error", "job": 1}"#,
            r#"{"event": "cancelled"}"#,
            r#"{"event": "step", "image": "fill", "step": 1, "steps": 1, "elapsed": 1}"#,
        ] {
            assert_eq!(WorkerEvent::parse(line), None, "{line}");
        }
    }

    /// A cold 512² job measured with the real 9B model and one step on the
    /// reference machine: event times since the worker was started, then
    /// the cutout.
    #[test]
    fn the_estimate_refines_from_measured_steps_and_never_goes_backwards() {
        use WorkerImage::{Fill, LineArt};
        let started = Instant::now();
        let at = |seconds: f64| started + Duration::from_secs_f64(seconds);
        let cold = estimate::cold_start_seconds();
        let mut tracker = Tracker::new(
            (512, 512),
            1,
            0,
            (cold, true),
            Calibration::default(),
            started,
        );
        let initial = tracker.progress(started);
        assert_eq!(initial.fraction, 0.);
        assert_eq!(
            initial.remaining,
            estimate::estimate((512, 512), 0, cold, true)
        );
        let end = 17.35 + CUTOUT_SECONDS;
        let step = |image, elapsed| WorkerEvent::Step {
            job: 1,
            image,
            step: 1,
            steps: 1,
            elapsed,
        };
        let timeline = [
            (4.03, WorkerEvent::Load),
            (6.52, WorkerEvent::Ready),
            (12.24, step(LineArt, 5.705)),
            (
                12.24,
                WorkerEvent::Decode {
                    job: 1,
                    image: LineArt,
                },
            ),
            (16.99, step(Fill, 4.268)),
            (
                16.99,
                WorkerEvent::Decode {
                    job: 1,
                    image: Fill,
                },
            ),
            (17.35, WorkerEvent::Done { job: 1 }),
        ];
        let mut previous = initial.fraction;
        for (seconds, event) in &timeline {
            // Between events, the remaining time counts down.
            let before = tracker.progress(at(seconds - 0.05));
            assert!(before.fraction >= previous);
            tracker.observe(event, at(*seconds));
            let progress = tracker.progress(at(*seconds));
            assert!(progress.fraction >= before.fraction, "{event:?}");
            let actual = end - seconds;
            let error = (progress.remaining.as_secs_f64() - actual).abs();
            // With one step, the first step's time scales the model.
            assert!(
                error < 0.15 * actual + 1.,
                "{event:?}: {progress:?}, actual {actual:.2} s"
            );
            assert_eq!(progress.stage, Stage::Generating);
            previous = progress.fraction;
        }
        // The job is done, but the bar is not until the cutout is.
        tracker.cutting_out(at(17.35));
        let cutting_out = tracker.progress(at(17.35));
        assert_eq!(cutting_out.stage, Stage::CuttingOut);
        assert_eq!(
            cutting_out.remaining,
            Duration::from_secs_f64(CUTOUT_SECONDS)
        );
        assert!((previous..MAX_RUNNING_FRACTION).contains(&cutting_out.fraction));
        let late = tracker.progress(at(end + 5.));
        assert_eq!(late.remaining, Duration::ZERO);
        assert!(late.fraction < 1.);
        tracker.finish(at(end));
        assert_eq!(tracker.progress(at(end)).fraction, 1.);
        assert_eq!(tracker.progress(at(end)).remaining, Duration::ZERO);
        assert_eq!(
            tracker.predicted(),
            estimate::estimate((512, 512), 0, cold, true)
        );
    }

    #[test]
    fn a_needed_encoding_is_in_the_first_estimate_and_its_stage() {
        let started = Instant::now();
        let mut tracker = Tracker::new(
            (512, 512),
            1,
            2,
            (0., true),
            Calibration::default(),
            started,
        );
        assert_eq!(tracker.prompts_encoded(), 2);
        assert_eq!(
            tracker.progress(started).remaining,
            estimate::estimate((512, 512), 2, 0., true)
        );
        // The encoding process reports its stage after its own start-up;
        // a warm worker then only needs the job.
        let at = started + Duration::from_secs_f64(STARTUP_SECONDS);
        tracker.observe(&WorkerEvent::Encode { prompts: 2 }, at);
        let remaining = tracker.progress(at).remaining.as_secs_f64();
        let expected = estimate::estimate((512, 512), 2, 0., true).as_secs_f64() - STARTUP_SECONDS;
        assert!(
            (remaining - expected).abs() < 1e-6,
            "{remaining} vs {expected}"
        );
    }

    #[test]
    fn running_progress_stays_below_complete_and_counts_down_to_zero() {
        let started = Instant::now();
        let mut tracker = Tracker::new(
            (160, 160),
            1,
            0,
            (0., true),
            Calibration::default(),
            started,
        );
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
        let mut tracker = Tracker::new((512, 512), 2, 0, (0., true), slow, started);
        let expected = slow.factor() * estimate::estimate((512, 512), 0, 0., true).as_secs_f64();
        assert!((tracker.progress(started).remaining.as_secs_f64() - expected).abs() < 1e-6);
        // Measured steps replace the calibrated model.
        let at = |seconds: f64| started + Duration::from_secs_f64(seconds);
        tracker.observe(&step(WorkerImage::LineArt, 1, 2.9), at(6.6));
        tracker.observe(&step(WorkerImage::LineArt, 2, 4.6), at(8.3));
        let measured = tracker.remaining_after(WorkerImage::LineArt, 2, 1.7);
        assert!((tracker.progress(at(8.3)).remaining.as_secs_f64() - measured).abs() < 1e-6);
    }
}
