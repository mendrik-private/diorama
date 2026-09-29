//! Process plumbing shared by the local inference workers (LaMa, line art and
//! BiRefNet): Flatpak host launch, setup-recorded runtime configuration, the
//! cache directory, a logged spawn, streamed stdout lines, and a cancellable,
//! time-limited wait.
use std::{
    fs::{self, File},
    io::{self, BufRead, BufReader, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    sync::{
        Mutex, MutexGuard, TryLockError,
        mpsc::{self, Receiver, RecvTimeoutError},
    },
    thread,
    time::{Duration, Instant},
};

use crate::{document::CancellationToken, error::AppError};

/// Model workers share one permit because inference can exceed several GiB.
static INFERENCE: Mutex<()> = Mutex::new(());

const LOG_TAIL_BYTES: u64 = 8 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Launch {
    Direct,
    FlatpakHost { launcher: PathBuf },
}

impl Launch {
    pub(super) fn detect() -> Self {
        Self::from_marker(Path::new("/.flatpak-info"))
    }

    fn from_marker(flatpak_info: &Path) -> Self {
        if flatpak_info.is_file() {
            Self::FlatpakHost {
                launcher: PathBuf::from("flatpak-spawn"),
            }
        } else {
            Self::Direct
        }
    }

    /// A command running `program` directly or on the Flatpak host. Host
    /// launches strip the sandbox loader environment and restore only the
    /// library path a setup script recorded on the host.
    pub(super) fn command(&self, program: &Path, host_library_path: Option<&str>) -> Command {
        match self {
            Self::Direct => Command::new(program),
            Self::FlatpakHost { launcher } => {
                let mut command = Command::new(launcher);
                command.args([
                    "--host",
                    "--watch-bus",
                    "--unset-env=LD_LIBRARY_PATH",
                    "--unset-env=LD_PRELOAD",
                ]);
                if let Some(library_path) = host_library_path {
                    command.arg(format!("--env=LD_LIBRARY_PATH={library_path}"));
                }
                command.arg(program);
                command
            }
        }
    }

    /// Where a setup script records its runtime. Setup normally runs on the
    /// host, so Flatpak uses the host's config location rather than its
    /// app-scoped `XDG_CONFIG_HOME`.
    pub(super) fn runtime_config_path(&self, filename: &str) -> Option<PathBuf> {
        let home = std::env::var_os("HOME").map(PathBuf::from)?;
        Some(match self {
            Self::FlatpakHost { .. } => home.join(".config/diorama").join(filename),
            Self::Direct => std::env::var_os("XDG_CONFIG_HOME")
                .map(PathBuf::from)
                .filter(|path| path.is_absolute())
                .unwrap_or_else(|| home.join(".config"))
                .join("diorama")
                .join(filename),
        })
    }
}

/// `key=value` lines recorded by a setup script.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct RuntimeConfiguration {
    pub(super) python: Option<PathBuf>,
    pub(super) library_path: Option<String>,
}

impl RuntimeConfiguration {
    /// A missing or unreadable file is an empty configuration.
    pub(super) fn read(path: Option<&Path>) -> Self {
        path.and_then(|path| fs::read_to_string(path).ok())
            .map(|contents| Self::parse(&contents))
            .unwrap_or_default()
    }

    fn parse(contents: &str) -> Self {
        let mut configuration = Self::default();
        for line in contents.lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let value = value.trim();
            if value.is_empty() {
                continue;
            }
            match key.trim() {
                "python" => configuration.python = Some(PathBuf::from(value)),
                "library_path" => configuration.library_path = Some(value.to_owned()),
                _ => {}
            }
        }
        configuration
    }
}

/// `$XDG_CACHE_HOME` when absolute, otherwise `~/.cache`. In Flatpak this is
/// the app cache, which is also visible to host-launched workers.
pub(super) fn cache_home() -> Option<PathBuf> {
    std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(host_cache_home)
}

/// The host's `~/.cache`, where host-run setup scripts install models.
pub(super) fn host_cache_home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache"))
}

/// Prefer an app-managed install when `present`, otherwise reuse one a host
/// setup command installed, so Flatpak does not need a second download.
/// Falls back to the app location so errors name where setup installs.
pub(super) fn app_or_host_install(
    cache: &Path,
    host_cache: Option<&Path>,
    relative: &Path,
    present: impl Fn(&Path) -> bool,
) -> PathBuf {
    let app = cache.join(relative);
    if present(&app) {
        return app;
    }
    host_cache
        .map(|cache| cache.join(relative))
        .filter(|host| present(host))
        .unwrap_or(app)
}

/// Why a worker stopped early. The child process has already been killed and
/// reaped.
#[derive(Debug)]
pub(super) enum WaitError {
    Cancelled(AppError),
    TimedOut,
    Io(io::Error),
}

impl WaitError {
    pub(super) fn into_app_error(self, timed_out: AppError) -> AppError {
        match self {
            Self::Cancelled(error) => error,
            Self::TimedOut => timed_out,
            Self::Io(error) => error.into(),
        }
    }
}

/// Serialize model inference across windows and tools. Another model may run
/// for minutes, so the wait has no deadline; only cancellation ends it. A
/// worker's own timeout starts once it holds the permit.
pub(super) fn inference_permit(
    cancellation: &CancellationToken,
) -> crate::error::Result<MutexGuard<'static, ()>> {
    loop {
        cancellation.check()?;
        match INFERENCE.try_lock() {
            Ok(permit) => return Ok(permit),
            Err(TryLockError::Poisoned(error)) => return Ok(error.into_inner()),
            Err(TryLockError::WouldBlock) => thread::sleep(Duration::from_millis(20)),
        }
    }
}

/// Write an executable fake worker for tests. The script is written to a
/// staging file and copied by a `cp` child, so this process never holds a
/// writable descriptor to the executed inode. Otherwise a concurrent test's
/// fork could inherit that descriptor and make this exec fail with
/// `ETXTBSY`.
#[cfg(all(test, unix))]
pub(super) fn write_test_executable(path: &Path, script: &str) {
    use std::os::unix::fs::PermissionsExt;
    let name = path.file_name().expect("executable file name");
    let staging = path.with_file_name(format!(".{}.staging", name.to_string_lossy()));
    fs::write(&staging, script).unwrap();
    let copied = Command::new("cp").arg(&staging).arg(path).status().unwrap();
    assert!(copied.success(), "copy fake executable");
    fs::remove_file(&staging).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

/// Spawn with stdin closed and stdout/stderr appended to `log`.
pub(super) fn spawn_logged(command: &mut Command, log: &Path) -> io::Result<Child> {
    let log = File::create(log)?;
    command
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log)
        .spawn()
}

/// Spawn with stdin closed, stdout piped for [`StdoutLines`], and stderr
/// appended to `log`.
pub(super) fn spawn_streaming(command: &mut Command, log: &Path) -> io::Result<Child> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(File::create(log)?)
        .spawn()
}

/// The lines a child writes to its piped stdout, read on a background thread
/// so that waiting never blocks on the pipe.
pub(super) struct StdoutLines(Receiver<String>);

impl StdoutLines {
    /// Take the child's piped stdout; `None` if it is not piped.
    pub(super) fn take(child: &mut Child) -> Option<Self> {
        let stdout = child.stdout.take()?;
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            let mut line = Vec::new();
            loop {
                line.clear();
                match reader.read_until(b'\n', &mut line) {
                    Ok(0) | Err(_) => return,
                    Ok(_) => {
                        let text = String::from_utf8_lossy(&line).trim_end().to_owned();
                        if sender.send(text).is_err() {
                            return;
                        }
                    }
                }
            }
        });
        Some(Self(receiver))
    }

    /// Lines received so far, without blocking.
    pub(super) fn pending(&self) -> impl Iterator<Item = String> + '_ {
        self.0.try_iter()
    }

    /// The remaining lines once the child has exited. A descendant that
    /// inherited the pipe may keep it open, so this waits at most `grace`.
    pub(super) fn finish(self, grace: Duration) -> Vec<String> {
        let deadline = Instant::now() + grace;
        let mut lines = Vec::new();
        loop {
            match self
                .0
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            {
                Ok(line) => lines.push(line),
                Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => return lines,
            }
        }
    }
}

/// Wait for `child`, killing and reaping it on cancellation, timeout, or a
/// failed status query.
pub(super) fn wait(
    child: &mut Child,
    cancellation: &CancellationToken,
    timeout: Duration,
    poll_interval: Duration,
) -> std::result::Result<ExitStatus, WaitError> {
    wait_with(child, cancellation, timeout, poll_interval, || {})
}

/// [`wait`], calling `on_poll` on the waiting thread before every poll.
pub(super) fn wait_with(
    child: &mut Child,
    cancellation: &CancellationToken,
    timeout: Duration,
    poll_interval: Duration,
    mut on_poll: impl FnMut(),
) -> std::result::Result<ExitStatus, WaitError> {
    let started = Instant::now();
    loop {
        on_poll();
        if let Err(error) = cancellation.check() {
            kill_and_reap(child);
            return Err(WaitError::Cancelled(error));
        }
        if started.elapsed() >= timeout {
            kill_and_reap(child);
            return Err(WaitError::TimedOut);
        }
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) => thread::sleep(poll_interval),
            Err(error) => {
                kill_and_reap(child);
                return Err(WaitError::Io(error));
            }
        }
    }
}

pub(super) fn kill_and_reap(child: &mut Child) {
    // `kill` reports InvalidInput when `try_wait` raced with normal exit; the
    // following `wait` still reaps that child (or confirms it was already reaped).
    let _ = child.kill();
    let _ = child.wait();
}

/// The last 8 KiB of a worker log, for error messages.
pub(super) fn log_tail(path: &Path) -> String {
    let Ok(mut log) = File::open(path) else {
        return "<unavailable>".into();
    };
    let start = log
        .metadata()
        .map(|metadata| metadata.len().saturating_sub(LOG_TAIL_BYTES))
        .unwrap_or(0);
    if log.seek(SeekFrom::Start(start)).is_err() {
        return "<unavailable>".into();
    }
    let mut tail = Vec::new();
    if log.take(LOG_TAIL_BYTES).read_to_end(&mut tail).is_err() {
        return "<unavailable>".into();
    }
    // The cut may split a UTF-8 sequence; keep the rest of the tail readable.
    let mut tail = String::from_utf8_lossy(&tail).into_owned();
    if start > 0 {
        tail.insert_str(0, "…\n");
    }
    tail.trim().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launch_mode_detects_flatpak_explicitly() {
        let directory = tempfile::tempdir().unwrap();
        let marker = directory.path().join("flatpak-info");
        assert_eq!(Launch::from_marker(&marker), Launch::Direct);
        fs::write(&marker, "[Instance]\n").unwrap();
        assert_eq!(
            Launch::from_marker(&marker),
            Launch::FlatpakHost {
                launcher: PathBuf::from("flatpak-spawn"),
            }
        );
    }

    #[test]
    fn host_command_strips_sandbox_loader_and_restores_only_the_recorded_path() {
        let launch = Launch::FlatpakHost {
            launcher: PathBuf::from("/sandbox/bin/flatpak spawn"),
        };
        let command = launch.command(Path::new("/host/python with deps"), Some("/host/lib a:/b"));
        assert_eq!(
            command.get_program(),
            Path::new("/sandbox/bin/flatpak spawn")
        );
        assert_eq!(
            command
                .get_args()
                .map(|argument| argument.to_string_lossy().into_owned())
                .collect::<Vec<_>>(),
            [
                "--host",
                "--watch-bus",
                "--unset-env=LD_LIBRARY_PATH",
                "--unset-env=LD_PRELOAD",
                "--env=LD_LIBRARY_PATH=/host/lib a:/b",
                "/host/python with deps",
            ]
        );
        let direct = Launch::Direct.command(Path::new("/venv/python"), Some("/ignored"));
        assert_eq!(direct.get_program(), Path::new("/venv/python"));
        assert_eq!(direct.get_args().count(), 0);
    }

    #[test]
    fn configured_runtime_uses_recorded_values_without_normalizing_them() {
        let configuration = RuntimeConfiguration::parse(
            "# comment\npython= /venv/bin/python \nlibrary_path=/host/lib with spaces:/host/rocm/lib\nempty=\n",
        );
        assert_eq!(
            configuration.python,
            Some(PathBuf::from("/venv/bin/python"))
        );
        assert_eq!(
            configuration.library_path.as_deref(),
            Some("/host/lib with spaces:/host/rocm/lib")
        );
        assert_eq!(
            RuntimeConfiguration::read(Some(Path::new("/nonexistent/diorama.conf"))),
            RuntimeConfiguration::default()
        );
    }

    #[test]
    fn install_prefers_the_app_cache_then_an_existing_host_install() {
        let root = tempfile::tempdir().unwrap();
        let app_cache = root.path().join("app-cache");
        let host_cache = root.path().join("host-cache");
        let relative = Path::new("diorama/model");
        let present = |path: &Path| path.join("ready").is_file();
        assert_eq!(
            app_or_host_install(&app_cache, Some(&host_cache), relative, present),
            app_cache.join(relative)
        );
        fs::create_dir_all(host_cache.join(relative)).unwrap();
        fs::write(host_cache.join(relative).join("ready"), b"").unwrap();
        assert_eq!(
            app_or_host_install(&app_cache, Some(&host_cache), relative, present),
            host_cache.join(relative)
        );
        fs::create_dir_all(app_cache.join(relative)).unwrap();
        fs::write(app_cache.join(relative).join("ready"), b"").unwrap();
        assert_eq!(
            app_or_host_install(&app_cache, Some(&host_cache), relative, present),
            app_cache.join(relative)
        );
    }

    #[test]
    fn log_tail_is_bounded_and_tolerates_a_split_utf8_sequence() {
        let directory = tempfile::tempdir().unwrap();
        let log = directory.path().join("runtime.log");
        // 2-byte characters followed by an odd-length suffix put the 8 KiB
        // cut in the middle of a character.
        let mut contents = "é".repeat(LOG_TAIL_BYTES as usize);
        contents.push_str("final-line.");
        assert_eq!((contents.len() as u64 - LOG_TAIL_BYTES) % 2, 1);
        fs::write(&log, contents).unwrap();
        let tail = log_tail(&log);
        assert!(tail.starts_with('…'));
        assert!(tail.ends_with("final-line."));
        assert!(tail.len() <= LOG_TAIL_BYTES as usize + "…\n".len() + 3);
        assert_eq!(log_tail(&directory.path().join("missing")), "<unavailable>");
    }

    #[test]
    fn a_held_inference_permit_blocks_without_a_deadline_until_cancelled_or_released() {
        let held = inference_permit(&CancellationToken::default()).unwrap();
        let cancelled = CancellationToken::default();
        thread::scope(|scope| {
            let waiter = scope.spawn(|| inference_permit(&cancelled).map(drop));
            thread::sleep(Duration::from_millis(150));
            assert!(!waiter.is_finished(), "a held permit has no wait deadline");
            cancelled.cancel();
            assert!(matches!(waiter.join().unwrap(), Err(AppError::Cancelled)));

            let waiter = scope.spawn(|| inference_permit(&CancellationToken::default()).map(drop));
            thread::sleep(Duration::from_millis(50));
            assert!(!waiter.is_finished());
            drop(held);
            waiter.join().unwrap().unwrap();
        });
    }

    #[cfg(unix)]
    #[test]
    fn test_executables_run_while_other_threads_fork() {
        let directory = tempfile::tempdir().unwrap();
        thread::scope(|scope| {
            for worker in 0..8 {
                let directory = directory.path();
                scope.spawn(move || {
                    for run in 0..8 {
                        let script = directory.join(format!("fake-{worker}-{run}"));
                        write_test_executable(&script, "#!/bin/sh\nexit 4\n");
                        let status = Command::new(&script).status().unwrap();
                        assert_eq!(status.code(), Some(4));
                    }
                });
            }
        });
    }

    #[cfg(unix)]
    #[test]
    fn streamed_stdout_lines_arrive_while_waiting_and_stderr_goes_to_the_log() {
        let directory = tempfile::tempdir().unwrap();
        let log = directory.path().join("worker.log");
        let mut child = spawn_streaming(
            Command::new("sh").args([
                "-c",
                "echo first; echo warning >&2; sleep 0.2; printf 'second\\r\\nno newline'",
            ]),
            &log,
        )
        .unwrap();
        let lines = StdoutLines::take(&mut child).unwrap();
        assert!(StdoutLines::take(&mut child).is_none());
        let mut seen = Vec::new();
        let mut polls = 0;
        let status = wait_with(
            &mut child,
            &CancellationToken::default(),
            Duration::from_secs(5),
            Duration::from_millis(5),
            || {
                polls += 1;
                seen.extend(lines.pending());
            },
        )
        .unwrap();
        assert!(status.success());
        assert!(polls > 1);
        assert_eq!(seen.first().map(String::as_str), Some("first"));
        seen.extend(lines.finish(Duration::from_secs(5)));
        assert_eq!(seen, ["first", "second", "no newline"]);
        assert_eq!(fs::read_to_string(&log).unwrap(), "warning\n");
    }

    #[cfg(unix)]
    #[test]
    fn a_cancelled_streaming_wait_kills_the_child_and_ends_the_stream() {
        let directory = tempfile::tempdir().unwrap();
        let mut child = spawn_streaming(
            Command::new("sh").args(["-c", "echo started; exec sleep 60"]),
            &directory.path().join("worker.log"),
        )
        .unwrap();
        let lines = StdoutLines::take(&mut child).unwrap();
        let cancellation = CancellationToken::default();
        let started = Instant::now();
        let result = wait_with(
            &mut child,
            &cancellation,
            Duration::from_secs(30),
            Duration::from_millis(2),
            || {
                if lines.pending().any(|line| line == "started") {
                    cancellation.cancel();
                }
            },
        );
        assert!(matches!(
            result,
            Err(WaitError::Cancelled(AppError::Cancelled))
        ));
        assert!(started.elapsed() < Duration::from_secs(10));
        assert!(child.try_wait().unwrap().is_some(), "must be reaped");
        assert!(lines.finish(Duration::from_secs(5)).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn wait_reports_exit_timeout_and_cancellation() {
        let exited = wait(
            &mut Command::new("sh").args(["-c", "exit 3"]).spawn().unwrap(),
            &CancellationToken::default(),
            Duration::from_secs(5),
            Duration::from_millis(2),
        )
        .unwrap();
        assert_eq!(exited.code(), Some(3));

        let mut sleeping = Command::new("sleep").arg("60").spawn().unwrap();
        let started = Instant::now();
        assert!(matches!(
            wait(
                &mut sleeping,
                &CancellationToken::default(),
                Duration::from_millis(30),
                Duration::from_millis(2),
            ),
            Err(WaitError::TimedOut)
        ));
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(sleeping.try_wait().unwrap().is_some(), "must be reaped");

        let cancelled = CancellationToken::default();
        cancelled.cancel();
        let mut sleeping = Command::new("sleep").arg("60").spawn().unwrap();
        assert!(matches!(
            wait(
                &mut sleeping,
                &cancelled,
                Duration::from_secs(5),
                Duration::from_millis(2),
            ),
            Err(WaitError::Cancelled(AppError::Cancelled))
        ));
        assert!(sleeping.try_wait().unwrap().is_some(), "must be reaped");
    }
}
