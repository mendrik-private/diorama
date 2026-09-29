//! The resident generation worker.
//!
//! One worker process keeps the model loaded while Diorama expects Game
//! Asset work (see [`Residency::set_warm`]) and runs one job at a time from
//! JSON lines on its stdin. Its stdout carries the progress protocol of
//! [`WorkerEvent`] with per-job ends. A job whose caller was cancelled is
//! asked to stop; the next job waits for that, or replaces an unresponsive
//! worker. A worker that exits, crashes or stops responding is replaced by
//! the next job.
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command},
    sync::{
        Arc, LazyLock, Mutex, MutexGuard, TryLockError,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use tempfile::TempDir;

use super::{
    POLL_INTERVAL, PROGRESS_INTERVAL, RECIPE, Runtime, SETUP, WORKER,
    progress::{Progress, Tracker, WorkerEvent},
};
use crate::{
    document::CancellationToken,
    error::{AppError, Result},
    tools::worker_process::{self, StdoutLines},
};

/// A warm worker without jobs exits after this long, so an idle Diorama
/// does not hold several GB of GPU memory indefinitely.
pub(super) const IDLE_TIMEOUT: Duration = Duration::from_secs(10 * 60);
/// How long a cancelled job may take to stop before its worker is killed.
/// It stops at its next denoising step.
pub(super) const CANCEL_GRACE: Duration = Duration::from_secs(20);
/// How long a worker may take to exit after its stdin closed.
const STOP_GRACE: Duration = Duration::from_secs(3);

/// The process-wide resident worker.
pub(super) static RESIDENCY: LazyLock<Arc<Residency>> = LazyLock::new(Arc::default);

/// Whether the worker should stay loaded, and the worker itself. The worker
/// lock is held for a whole job, so jobs run one at a time.
#[derive(Default)]
pub(super) struct Residency {
    keep_warm: AtomicBool,
    worker: Mutex<Option<Resident>>,
}

/// What a worker was started with; a different runtime needs a new worker.
#[derive(Debug, Clone, PartialEq)]
struct Identity {
    python: PathBuf,
    model: PathBuf,
    gguf: PathBuf,
    device: Option<String>,
    launch: worker_process::Launch,
    host_library_path: Option<String>,
}

impl Identity {
    fn of(runtime: &Runtime) -> Self {
        Self {
            python: runtime.python.clone(),
            model: runtime.model.clone(),
            gguf: runtime.gguf.clone(),
            device: runtime.device.clone(),
            launch: runtime.launch.clone(),
            host_library_path: runtime.host_library_path.clone(),
        }
    }
}

/// One running worker process.
struct Resident {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: StdoutLines,
    /// Holds the worker script and its log; removed with the worker.
    _directory: TempDir,
    log: PathBuf,
    identity: Identity,
    spawned: Instant,
    ready: bool,
    /// The size of the latest job; another size warms up again.
    last_size: Option<(u32, u32)>,
    next_job: u32,
    /// A job whose caller was cancelled, and when the worker must have
    /// reported its end.
    abandoned: Option<(u32, Instant)>,
}

/// Why a job did not complete.
enum JobFailure {
    Cancelled,
    /// The worker reported the failure and can run the next job.
    Reported(String),
    /// The worker must be replaced.
    Fatal(AppError),
}

/// The files one job reads and writes, in a host-visible directory.
pub(super) struct Job<'a> {
    pub(super) reference: &'a Path,
    pub(super) size: (u32, u32),
    pub(super) line_art: &'a Path,
    pub(super) fill: &'a Path,
    pub(super) line_art_embeds: &'a Path,
    pub(super) fill_embeds: &'a Path,
}

impl Residency {
    /// Record whether the worker should stay loaded, and start or stop it
    /// in the background. `start` resolves the runtime and the cache
    /// directory; a failure only logs, since it surfaces when the user
    /// renders.
    pub(super) fn set_warm(
        self: &Arc<Self>,
        warm: bool,
        start: impl FnOnce() -> Result<(Runtime, PathBuf)> + Send + 'static,
    ) {
        self.keep_warm.store(warm, Ordering::SeqCst);
        let residency = self.clone();
        thread::spawn(move || residency.reconcile(start));
    }

    /// Start a worker if one is wanted and missing, or stop an unwanted one.
    /// Waits for a running job to finish first.
    pub(super) fn reconcile(&self, start: impl FnOnce() -> Result<(Runtime, PathBuf)>) {
        let mut worker = self
            .worker
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if !self.keep_warm.load(Ordering::SeqCst) {
            let stopped = worker.take();
            drop(worker);
            if let Some(resident) = stopped {
                resident.stop();
            }
            return;
        }
        if worker.as_mut().is_some_and(Resident::alive) {
            return;
        }
        let started = start().and_then(|(runtime, cache)| {
            super::verify_model(&runtime.model)?;
            super::verify_gguf(&runtime.gguf)?;
            Resident::spawn(&runtime, &cache)
        });
        match started {
            Ok(resident) => *worker = Some(resident),
            Err(error) => tracing::debug!(%error, "Could not warm up the line-art worker"),
        }
    }

    #[cfg(test)]
    pub(super) fn set_keep_warm(&self, warm: bool) {
        self.keep_warm.store(warm, Ordering::SeqCst);
    }

    /// Whether a worker is running; for tests.
    #[cfg(test)]
    pub(super) fn is_running(&self) -> bool {
        self.worker
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .as_mut()
            .is_some_and(Resident::alive)
    }

    /// The worker lock, waiting as long as another job runs; only
    /// cancellation ends the wait.
    fn lock(&self, cancellation: &CancellationToken) -> Result<MutexGuard<'_, Option<Resident>>> {
        loop {
            cancellation.check()?;
            match self.worker.try_lock() {
                Ok(worker) => return Ok(worker),
                Err(TryLockError::Poisoned(error)) => return Ok(error.into_inner()),
                Err(TryLockError::WouldBlock) => thread::sleep(Duration::from_millis(20)),
            }
        }
    }

    /// Run one job on the resident worker, starting one if needed.
    /// `before_start` runs, under the worker lock, before a cold worker is
    /// started; it receives the seconds until a worker would be ready (0 if
    /// it is ready) and whether it has yet to generate at this size, and
    /// returns the tracker for the job.
    pub(super) fn run(
        &self,
        runtime: &Runtime,
        cache: &Path,
        job: &Job<'_>,
        cancellation: &CancellationToken,
        progress: &dyn Fn(Progress),
        before_start: impl FnOnce((f64, bool)) -> Result<Tracker>,
    ) -> Result<Tracker> {
        let mut worker = self.lock(cancellation)?;
        let identity = Identity::of(runtime);
        if let Some(resident) = worker.as_mut() {
            let reusable = resident.alive()
                && resident.identity == identity
                && resident.settle(cancellation)?;
            if !reusable {
                // Dropping kills it.
                worker.take();
            }
        }
        let start = match worker.as_mut() {
            Some(resident) => (
                resident.start_up_seconds(),
                resident.last_size != Some(job.size),
            ),
            None => (super::estimate::cold_start_seconds(), true),
        };
        let mut tracker = before_start(start)?;
        cancellation.check()?;
        if worker.is_none() {
            *worker = Some(Resident::spawn(runtime, cache)?);
        }
        let resident = worker.as_mut().expect("a worker was just ensured");
        let result = match resident.run(job, runtime, cancellation, &mut tracker, progress) {
            Ok(()) => Ok(()),
            Err(JobFailure::Cancelled) => {
                // An unwanted worker is killed at once rather than awaited.
                if !self.keep_warm.load(Ordering::SeqCst) {
                    worker.take();
                }
                Err(AppError::Cancelled)
            }
            Err(JobFailure::Reported(message)) => Err(AppError::SketchGeneration(format!(
                "The local line-art model failed: {message}"
            ))),
            Err(JobFailure::Fatal(error)) => {
                worker.take();
                Err(error)
            }
        };
        if !self.keep_warm.load(Ordering::SeqCst)
            && result.is_ok()
            && let Some(resident) = worker.take()
        {
            resident.stop();
        }
        result.map(|()| tracker)
    }
}

impl Resident {
    fn spawn(runtime: &Runtime, cache: &Path) -> Result<Self> {
        fs::create_dir_all(cache)?;
        // The app cache is host-visible for Flatpak workers while private
        // /tmp is not.
        let directory = tempfile::Builder::new()
            .prefix(".resident-")
            .tempdir_in(cache)?;
        let script = directory.path().join("line_art_worker.py");
        fs::write(&script, WORKER)?;
        let log = directory.path().join("line-art.log");
        let mut child = worker_process::spawn_streaming(&mut serve_command(runtime, &script), &log)
            .map_err(|error| {
                AppError::SketchGeneration(format!(
                    "Could not start the line-art worker with {}: {error}. Run {SETUP} or set DIORAMA_LINE_ART_PYTHON",
                    runtime.python.display()
                ))
            })?;
        let stdin = child.stdin.take();
        let lines = StdoutLines::take(&mut child).expect("the worker's stdout is piped");
        Ok(Self {
            child,
            stdin,
            lines,
            _directory: directory,
            log,
            identity: Identity::of(runtime),
            spawned: Instant::now(),
            ready: false,
            last_size: None,
            next_job: 1,
            abandoned: None,
        })
    }

    fn alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// Seconds until the worker is ready, as far as can be told.
    fn start_up_seconds(&mut self) -> f64 {
        self.pending_events(|_| {});
        if self.ready {
            0.
        } else {
            (super::estimate::cold_start_seconds() - self.spawned.elapsed().as_secs_f64()).max(0.)
        }
    }

    /// Parse the events received so far, noting readiness and the end of
    /// an abandoned job, and pass them on.
    fn pending_events(&mut self, mut handle: impl FnMut(WorkerEvent)) {
        let events = self
            .lines
            .pending()
            .filter_map(|line| {
                let event = WorkerEvent::parse(&line);
                if event.is_none() {
                    tracing::debug!(line, "Ignoring a line-art worker line that is no event");
                }
                event
            })
            .collect::<Vec<_>>();
        for event in events {
            if event == WorkerEvent::Ready {
                self.ready = true;
            }
            if let Some((abandoned, _)) = self.abandoned
                && event.job() == Some(abandoned)
                && matches!(
                    event,
                    WorkerEvent::Done { .. }
                        | WorkerEvent::Failed { .. }
                        | WorkerEvent::Cancelled { .. }
                )
            {
                self.abandoned = None;
            }
            handle(event);
        }
    }

    /// Wait for an abandoned job to end. `false` if the worker did not end
    /// it within its grace period or exited, so it must be replaced.
    fn settle(&mut self, cancellation: &CancellationToken) -> Result<bool> {
        loop {
            self.pending_events(|_| {});
            let Some((_, deadline)) = self.abandoned else {
                return Ok(true);
            };
            if Instant::now() >= deadline || !self.alive() {
                return Ok(false);
            }
            cancellation.check()?;
            thread::sleep(POLL_INTERVAL);
        }
    }

    fn send(&mut self, message: &str) -> std::io::Result<()> {
        let stdin = self
            .stdin
            .as_mut()
            .ok_or_else(|| std::io::Error::other("the worker's stdin is closed"))?;
        stdin.write_all(message.as_bytes())?;
        stdin.write_all(b"\n")?;
        stdin.flush()
    }

    fn stopped(&self, status: Option<std::process::ExitStatus>) -> AppError {
        AppError::SketchGeneration(format!(
            "The local line-art model stopped{}: {}",
            status.map_or_else(String::new, |status| format!(" ({status})")),
            worker_process::log_tail(&self.log)
        ))
    }

    fn run(
        &mut self,
        job: &Job<'_>,
        runtime: &Runtime,
        cancellation: &CancellationToken,
        tracker: &mut Tracker,
        progress: &dyn Fn(Progress),
    ) -> std::result::Result<(), JobFailure> {
        let id = self.next_job;
        self.next_job += 1;
        self.last_size = Some(job.size);
        if self.send(&job_request(id, job)).is_err() {
            let status = self.child.try_wait().ok().flatten();
            return Err(JobFailure::Fatal(self.stopped(status)));
        }
        let started = Instant::now();
        let mut reported = started;
        loop {
            let now = Instant::now();
            if cancellation.check().is_err() {
                let _ = self.send(&format!("{{\"cancel\": {id}}}"));
                self.abandoned = Some((id, now + runtime.cancel_grace));
                return Err(JobFailure::Cancelled);
            }
            if now.duration_since(started) >= runtime.timeout {
                return Err(JobFailure::Fatal(AppError::SketchGeneration(format!(
                    "Local line art timed out after {} minutes",
                    runtime.timeout.as_secs() / 60
                ))));
            }
            let mut end = None;
            let mut observed = false;
            self.pending_events(|event| {
                if event.job().is_some_and(|job| job != id) {
                    return;
                }
                observed = true;
                match &event {
                    WorkerEvent::Done { .. } => end = Some(Ok(())),
                    WorkerEvent::Failed { message, .. } => {
                        end = Some(Err(JobFailure::Reported(message.clone())));
                    }
                    WorkerEvent::Cancelled { .. } => end = Some(Err(JobFailure::Cancelled)),
                    _ => {}
                }
                tracker.observe(&event, now);
            });
            if let Some(end) = end {
                return end;
            }
            if observed || now.duration_since(reported) >= PROGRESS_INTERVAL {
                reported = now;
                progress(tracker.progress(now));
            }
            match self.child.try_wait() {
                Ok(None) => thread::sleep(POLL_INTERVAL),
                Ok(Some(status)) => {
                    // Events written just before exiting still count.
                    thread::sleep(POLL_INTERVAL);
                    let mut reported_end = None;
                    self.pending_events(|event| match event {
                        WorkerEvent::Done { job } if job == id => reported_end = Some(Ok(())),
                        WorkerEvent::Failed { job, message } if job == id => {
                            reported_end = Some(Err(message));
                        }
                        _ => {}
                    });
                    return match reported_end {
                        Some(Ok(())) => Ok(()),
                        // The worker gave up on itself, e.g. out of memory.
                        Some(Err(message)) => Err(JobFailure::Fatal(AppError::SketchGeneration(
                            format!("The local line-art model failed: {message}"),
                        ))),
                        None => Err(JobFailure::Fatal(self.stopped(Some(status)))),
                    };
                }
                Err(error) => return Err(JobFailure::Fatal(error.into())),
            }
        }
    }

    /// Close stdin, which ends the worker, and reap it; it is killed if it
    /// does not exit in time.
    fn stop(mut self) {
        drop(self.stdin.take());
        let deadline = Instant::now() + STOP_GRACE;
        while Instant::now() < deadline && self.alive() {
            thread::sleep(POLL_INTERVAL);
        }
    }
}

/// A dropped worker is killed and reaped, so it never outlives its owner.
impl Drop for Resident {
    fn drop(&mut self) {
        drop(self.stdin.take());
        worker_process::kill_and_reap(&mut self.child);
    }
}

/// Start the resident worker.
pub(super) fn serve_command(runtime: &Runtime, script: &Path) -> Command {
    let mut command = runtime
        .launch
        .command(&runtime.python, runtime.host_library_path.as_deref());
    command
        .arg(script)
        .arg("--serve")
        .arg("--model")
        .arg(&runtime.model)
        .arg("--gguf")
        .arg(&runtime.gguf)
        .arg("--idle-timeout")
        .arg(runtime.idle_timeout.as_secs().to_string());
    if let Some(device) = &runtime.device {
        command.arg("--device").arg(device);
    }
    command
}

/// One job as the worker reads it: a JSON object on one line.
fn job_request(id: u32, job: &Job<'_>) -> String {
    let path = |path: &Path| json_string(&path.to_string_lossy());
    format!(
        "{{\"job\": {id}, \"image\": {}, \"width\": {}, \"height\": {}, \"steps\": {}, \"guidance\": {}, \"line_art_embeds\": {}, \"line_art_seed\": {}, \"line_art_output\": {}, \"fill_embeds\": {}, \"fill_seed\": {}, \"fill_output\": {}}}",
        path(job.reference),
        job.size.0,
        job.size.1,
        RECIPE.steps,
        RECIPE.guidance,
        path(job.line_art_embeds),
        RECIPE.line_art_seed,
        path(job.line_art),
        path(job.fill_embeds),
        RECIPE.fill_seed,
        path(job.fill),
    )
}

/// `text` as a JSON string literal.
fn json_string(text: &str) -> String {
    let mut quoted = String::with_capacity(text.len() + 2);
    quoted.push('"');
    for c in text.chars() {
        match c {
            '"' => quoted.push_str("\\\""),
            '\\' => quoted.push_str("\\\\"),
            '\n' => quoted.push_str("\\n"),
            '\r' => quoted.push_str("\\r"),
            '\t' => quoted.push_str("\\t"),
            c if u32::from(c) < 0x20 => quoted.push_str(&format!("\\u{:04x}", u32::from(c))),
            c => quoted.push(c),
        }
    }
    quoted.push('"');
    quoted
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jobs_are_json_lines_with_escaped_paths() {
        let request = job_request(
            7,
            &Job {
                reference: Path::new("/cache/run \"a\"/reference.png"),
                size: (512, 528),
                line_art: Path::new("/c/l.png"),
                fill: Path::new("/c/f.png"),
                line_art_embeds: Path::new("/m/.l\\x.safetensors"),
                fill_embeds: Path::new("/m/.f.safetensors"),
            },
        );
        assert!(!request.contains('\n'));
        assert_eq!(
            request,
            format!(
                r#"{{"job": 7, "image": "/cache/run \"a\"/reference.png", "width": 512, "height": 528, "steps": {}, "guidance": 1.0, "line_art_embeds": "/m/.l\\x.safetensors", "line_art_seed": 0, "line_art_output": "/c/l.png", "fill_embeds": "/m/.f.safetensors", "fill_seed": 0, "fill_output": "/c/f.png"}}"#,
                RECIPE.steps
            )
        );
        assert_eq!(json_string("a\u{1}b\tc"), r#""a\u0001b\tc""#);
    }
}
