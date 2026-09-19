//! Native, short-lived inference worker protocol.
//!
//! Model execution runs in Diorama's own executable.  Keeping it in a child
//! process preserves the selection tool's cancellation guarantee without
//! depending on a host-installed interpreter or inference executable.

use std::{
    env,
    fs::{self, File},
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{Mutex, MutexGuard, TryLockError},
    thread,
    time::{Duration, Instant},
};

use diorama_inference::Backend;
use image::{GrayImage, RgbaImage};

use crate::{
    document::CancellationToken,
    error::{AppError, Result},
};

const WORKER_FLAG: &str = "--diorama-native-inference";
const GPU_FAILED: i32 = 42;
const LOG_TAIL_BYTES: u64 = 8 * 1024;
static INFERENCE: Mutex<()> = Mutex::new(());

enum AttemptError {
    App(AppError),
    Gpu(AppError),
}

struct WorkerFiles<'a> {
    input: &'a Path,
    output: &'a Path,
    log: &'a Path,
}

impl AttemptError {
    fn into_app(self) -> AppError {
        match self {
            Self::App(error) | Self::Gpu(error) => error,
        }
    }
}

const BIREFNET_NAME: &str = "BiRefNet";
const BIREFNET_TIMEOUT: Duration = Duration::from_secs(600);

fn birefnet_error(message: impl Into<String>) -> AppError {
    AppError::BackgroundRemoval(message.into())
}

/// Runs the model-worker mode when it was explicitly selected on the command
/// line.  The GTK application is never initialized in this mode.
pub fn run_worker_from_args() -> Option<glib::ExitCode> {
    let mut args = env::args_os();
    let _program = args.next()?;
    if args.next().as_deref() != Some(std::ffi::OsStr::new(WORKER_FLAG)) {
        return None;
    }
    let status = match WorkerRequest::parse(args) {
        Ok(request) => {
            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| request.execute())) {
                Ok(Ok(())) => 0,
                Ok(Err(error)) => {
                    eprintln!("{error}");
                    if request.backend == Backend::Gpu {
                        GPU_FAILED
                    } else {
                        1
                    }
                }
                Err(_) => {
                    eprintln!("native {BIREFNET_NAME} worker panicked");
                    if request.backend == Backend::Gpu {
                        GPU_FAILED
                    } else {
                        1
                    }
                }
            }
        }
        Err(error) => {
            eprintln!("{error}");
            1
        }
    };
    Some(glib::ExitCode::from(status as u8))
}

/// Runs BiRefNet in a cancellable native child worker.
pub fn birefnet_mask(image: &RgbaImage, cancellation: &CancellationToken) -> Result<GrayImage> {
    cancellation.check()?;
    let model = model_path()?;
    run(image, &model, cancellation).map(image::DynamicImage::into_luma8)
}

fn model_path() -> Result<PathBuf> {
    const VARIABLE: &str = "DIORAMA_BIREFNET_MODEL";
    const FILENAME: &str = "BiRefNet-F16.gguf";
    let configured = match env::var_os(VARIABLE).map(PathBuf::from) {
        Some(path) if !path.is_absolute() => {
            return Err(birefnet_error(format!(
                "{VARIABLE} must be an absolute path"
            )));
        }
        configured => configured,
    };
    let executable_root = env::current_exe().ok().and_then(|executable| {
        executable
            .parent()?
            .parent()
            .map(|prefix| prefix.join("share/diorama/models"))
    });
    let configured_root = match env::var_os("DIORAMA_MODELS_DIR").map(PathBuf::from) {
        Some(path) if !path.is_absolute() => {
            return Err(birefnet_error(
                "DIORAMA_MODELS_DIR must be an absolute path",
            ));
        }
        configured => configured,
    };
    let roots = configured_root
        .into_iter()
        .chain(
            env::var_os("XDG_DATA_HOME")
                .map(PathBuf::from)
                .filter(|path| path.is_absolute())
                .map(|data| data.join("diorama/models")),
        )
        .chain(
            env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share/diorama/models")),
        )
        .chain(executable_root)
        .chain([PathBuf::from("/app/share/diorama/models")]);
    let model = configured.or_else(|| {
        roots
            .map(|root| root.join(FILENAME))
            .find(|path| path.is_file())
    });
    model.filter(|path| path.is_file()).ok_or_else(|| {
        birefnet_error(format!(
            "{BIREFNET_NAME} model is unavailable; set {VARIABLE} to its absolute GGUF path"
        ))
    })
}

fn run(
    image: &RgbaImage,
    model: &Path,
    cancellation: &CancellationToken,
) -> Result<image::DynamicImage> {
    cancellation.check()?;
    let deadline = Instant::now() + BIREFNET_TIMEOUT;
    let _permit = acquire_permit(deadline, cancellation)?;
    let root = cache_directory()?;
    fs::create_dir_all(&root)?;
    let directory = tempfile::Builder::new()
        .prefix("native-inference-")
        .tempdir_in(root)?;
    let input = directory.path().join("image.png");
    let output = directory.path().join("output.png");
    let log = directory.path().join("worker.log");
    let files = WorkerFiles {
        input: &input,
        output: &output,
        log: &log,
    };
    image.save(&input)?;
    run_with_gpu_fallback(deadline, cancellation, |backend, deadline| {
        run_once(
            image.dimensions(),
            model,
            &files,
            backend,
            deadline,
            cancellation,
        )
    })?;
    let output = image::open(output).map_err(AppError::from)?;
    cancellation.check()?;
    Ok(output)
}

fn acquire_permit(
    deadline: Instant,
    cancellation: &CancellationToken,
) -> Result<MutexGuard<'static, ()>> {
    loop {
        cancellation.check()?;
        match INFERENCE.try_lock() {
            Ok(permit) => return Ok(permit),
            Err(TryLockError::Poisoned(error)) => return Ok(error.into_inner()),
            Err(TryLockError::WouldBlock) if Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(20));
            }
            Err(TryLockError::WouldBlock) => {
                return Err(birefnet_error(format!(
                    "timed out waiting for native {BIREFNET_NAME}"
                )));
            }
        }
    }
}

fn run_with_gpu_fallback<T>(
    deadline: Instant,
    cancellation: &CancellationToken,
    mut attempt: impl FnMut(Backend, Instant) -> std::result::Result<T, AttemptError>,
) -> Result<T> {
    match attempt(Backend::Gpu, deadline) {
        Ok(output) => Ok(output),
        Err(AttemptError::Gpu(error)) => {
            cancellation.check()?;
            if Instant::now() >= deadline {
                return Err(error);
            }
            attempt(Backend::Cpu, deadline).map_err(AttemptError::into_app)
        }
        Err(error) => Err(error.into_app()),
    }
}

fn run_once(
    dimensions: (u32, u32),
    model: &Path,
    files: &WorkerFiles<'_>,
    backend: Backend,
    deadline: Instant,
    cancellation: &CancellationToken,
) -> std::result::Result<(), AttemptError> {
    cancellation.check().map_err(AttemptError::App)?;
    if Instant::now() >= deadline {
        return Err(AttemptError::App(birefnet_error(format!(
            "native {BIREFNET_NAME} timed out"
        ))));
    }
    let log_file = File::create(files.log)
        .map_err(AppError::from)
        .map_err(AttemptError::App)?;
    let mut child = worker_command(model, files.input, files.output, backend)
        .map_err(|error| AttemptError::App(birefnet_error(error)))?
        .stdin(Stdio::null())
        .stdout(
            log_file
                .try_clone()
                .map_err(AppError::from)
                .map_err(AttemptError::App)?,
        )
        .stderr(log_file)
        .spawn()
        .map_err(|error| {
            AttemptError::App(birefnet_error(format!(
                "could not start native {BIREFNET_NAME} worker: {error}",
            )))
        })?;
    supervise_child(
        dimensions,
        files.output,
        files.log,
        &mut child,
        backend,
        deadline,
        cancellation,
    )
}

fn supervise_child(
    dimensions: (u32, u32),
    output: &Path,
    log: &Path,
    child: &mut Child,
    backend: Backend,
    deadline: Instant,
    cancellation: &CancellationToken,
) -> std::result::Result<(), AttemptError> {
    loop {
        if let Err(error) = cancellation.check() {
            kill_and_reap(child);
            return Err(AttemptError::App(error));
        }
        if Instant::now() >= deadline {
            kill_and_reap(child);
            return Err(AttemptError::App(birefnet_error(format!(
                "native {BIREFNET_NAME} timed out"
            ))));
        }
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break,
            Ok(Some(status)) => {
                let error = birefnet_error(format!(
                    "native {BIREFNET_NAME} failed with {status}: {}",
                    log_tail(log)
                ));
                return Err(if backend == Backend::Gpu {
                    AttemptError::Gpu(error)
                } else {
                    AttemptError::App(error)
                });
            }
            Ok(None) => thread::sleep(Duration::from_millis(20)),
            Err(error) => {
                kill_and_reap(child);
                return Err(AttemptError::App(birefnet_error(format!(
                    "could not wait for native {BIREFNET_NAME}: {error}",
                ))));
            }
        }
    }
    let actual = image::image_dimensions(output)
        .map_err(AppError::from)
        .map_err(AttemptError::App)?;
    if actual != dimensions {
        return Err(AttemptError::App(AppError::InvalidDimensions));
    }
    cancellation.check().map_err(AttemptError::App)
}

fn worker_command(
    model: &Path,
    input: &Path,
    output: &Path,
    backend: Backend,
) -> std::result::Result<Command, String> {
    let executable =
        env::current_exe().map_err(|error| format!("cannot locate Diorama executable: {error}"))?;
    Ok(worker_command_for(
        executable, model, input, output, backend,
    ))
}

fn worker_command_for(
    executable: PathBuf,
    model: &Path,
    input: &Path,
    output: &Path,
    backend: Backend,
) -> Command {
    let mut command = Command::new(executable);
    command
        .arg(WORKER_FLAG)
        .arg("birefnet")
        .arg("--model")
        .arg(model)
        .arg("--input")
        .arg(input)
        .arg("--output")
        .arg(output)
        .arg("--backend")
        .arg(match backend {
            Backend::Cpu => "cpu",
            Backend::Gpu => "gpu",
        });
    command
}

fn cache_directory() -> Result<PathBuf> {
    let cache = env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))
        .ok_or_else(|| birefnet_error("HOME is unavailable; configure native model storage"))?;
    Ok(cache.join("diorama").join("native-inference"))
}

fn kill_and_reap(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn log_tail(path: &Path) -> String {
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
    let mut tail = String::new();
    if log.take(LOG_TAIL_BYTES).read_to_string(&mut tail).is_err() {
        return "<unavailable>".into();
    }
    if start > 0 {
        tail.insert_str(0, "…\n");
    }
    tail.trim().into()
}

#[derive(Debug)]
struct WorkerRequest {
    model: PathBuf,
    input: PathBuf,
    output: PathBuf,
    backend: Backend,
}

impl WorkerRequest {
    fn parse(args: impl Iterator<Item = std::ffi::OsString>) -> std::result::Result<Self, String> {
        let mut args = args;
        if args.next().as_deref() != Some(std::ffi::OsStr::new("birefnet")) {
            return Err("native worker requires `birefnet`".into());
        }
        let mut model = None;
        let mut input = None;
        let mut output = None;
        let mut backend = None;
        while let Some(flag) = args.next() {
            let value = args
                .next()
                .ok_or_else(|| format!("native worker argument {flag:?} needs a value"))?;
            match flag.to_string_lossy().as_ref() {
                "--model" => model = Some(PathBuf::from(value)),
                "--input" => input = Some(PathBuf::from(value)),
                "--output" => output = Some(PathBuf::from(value)),
                "--backend" => {
                    backend = Some(match value.to_string_lossy().as_ref() {
                        "cpu" => Backend::Cpu,
                        "gpu" => Backend::Gpu,
                        _ => return Err("native worker backend must be cpu or gpu".into()),
                    })
                }
                _ => return Err(format!("unknown native worker argument {flag:?}")),
            }
        }
        let request = Self {
            model: model.ok_or("native worker needs --model")?,
            input: input.ok_or("native worker needs --input")?,
            output: output.ok_or("native worker needs --output")?,
            backend: backend.ok_or("native worker needs --backend")?,
        };
        Ok(request)
    }

    fn execute(&self) -> std::result::Result<(), String> {
        let image = image::open(&self.input)
            .map_err(|error| error.to_string())?
            .into_rgba8();
        diorama_inference::birefnet::birefnet(&self.model, &image, self.backend)?
            .save(&self.output)
            .map_err(|error| error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use std::{
        ffi::OsString,
        fs::File,
        path::PathBuf,
        process::Command,
        thread,
        time::{Duration, Instant},
    };

    use image::{Rgba, RgbaImage};

    use super::{
        AttemptError, Backend, INFERENCE, WORKER_FLAG, WorkerRequest, acquire_permit,
        run_with_gpu_fallback, supervise_child, worker_command_for,
    };
    use crate::{document::CancellationToken, error::AppError};

    fn logged_child(script: &str, log: &std::path::Path) -> std::process::Child {
        let log = File::create(log).unwrap();
        Command::new("sh")
            .args(["-c", script])
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .spawn()
            .unwrap()
    }

    #[test]
    fn worker_command_keeps_paths_and_the_selected_backend_atomic() {
        let command = worker_command_for(
            PathBuf::from("/tmp/diorama worker"),
            std::path::Path::new("/tmp/model with spaces.gguf"),
            std::path::Path::new("/tmp/input image.png"),
            std::path::Path::new("/tmp/output image.png"),
            Backend::Cpu,
        );
        assert_eq!(
            command.get_program(),
            std::path::Path::new("/tmp/diorama worker")
        );
        assert_eq!(
            command
                .get_args()
                .map(|value| value.to_string_lossy())
                .collect::<Vec<_>>(),
            [
                WORKER_FLAG,
                "birefnet",
                "--model",
                "/tmp/model with spaces.gguf",
                "--input",
                "/tmp/input image.png",
                "--output",
                "/tmp/output image.png",
                "--backend",
                "cpu",
            ]
        );
    }

    #[test]
    fn worker_request_accepts_only_birefnet_without_a_mask() {
        let request = WorkerRequest::parse(
            [
                "birefnet",
                "--model",
                "model.gguf",
                "--input",
                "image.png",
                "--output",
                "out.png",
                "--backend",
                "cpu",
                "--mask",
                "forbidden.png",
            ]
            .into_iter()
            .map(OsString::from),
        );
        assert!(
            request
                .unwrap_err()
                .contains("unknown native worker argument")
        );

        let request = WorkerRequest::parse(
            [
                "lama",
                "--model",
                "model.gguf",
                "--input",
                "image.png",
                "--output",
                "out.png",
                "--backend",
                "gpu",
            ]
            .into_iter()
            .map(OsString::from),
        );
        assert!(request.unwrap_err().contains("requires `birefnet`"));
    }

    #[test]
    fn queued_permit_honours_cancellation() {
        let permit = INFERENCE.lock().unwrap_or_else(|error| error.into_inner());
        let cancellation = CancellationToken::default();
        let cancelling = cancellation.clone();
        let canceller = thread::spawn(move || {
            thread::sleep(Duration::from_millis(50));
            cancelling.cancel();
        });
        let result = acquire_permit(Instant::now() + Duration::from_secs(1), &cancellation);
        canceller.join().unwrap();
        drop(permit);
        assert!(matches!(result, Err(AppError::Cancelled)));
    }

    #[test]
    fn gpu_failure_retries_cpu_once_with_the_original_deadline() {
        let deadline = Instant::now() + Duration::from_secs(1);
        let mut calls = Vec::new();
        let output = run_with_gpu_fallback(
            deadline,
            &CancellationToken::default(),
            |backend, attempt_deadline| {
                calls.push((backend, attempt_deadline));
                if backend == Backend::Gpu {
                    Err(AttemptError::Gpu(AppError::BackgroundRemoval(
                        "gpu unavailable".into(),
                    )))
                } else {
                    Ok(7_u8)
                }
            },
        )
        .unwrap();
        assert_eq!(output, 7);
        assert_eq!(calls, [(Backend::Gpu, deadline), (Backend::Cpu, deadline)]);
    }

    #[test]
    fn cancellation_after_gpu_failure_does_not_retry_cpu() {
        let cancellation = CancellationToken::default();
        let mut calls = Vec::new();
        let result: std::result::Result<(), AppError> = run_with_gpu_fallback(
            Instant::now() + Duration::from_secs(1),
            &cancellation,
            |backend, _| {
                calls.push(backend);
                cancellation.cancel();
                Err(AttemptError::Gpu(AppError::BackgroundRemoval(
                    "gpu unavailable".into(),
                )))
            },
        );
        assert!(matches!(result, Err(AppError::Cancelled)));
        assert_eq!(calls, [Backend::Gpu]);
    }

    #[cfg(unix)]
    #[test]
    fn supervisor_reaps_an_active_worker_when_cancelled() {
        let root = tempfile::tempdir().unwrap();
        let output = root.path().join("output.png");
        let log = root.path().join("worker.log");
        let mut child = logged_child("exec sleep 60", &log);
        let token = CancellationToken::default();
        let cancelling = token.clone();
        let canceller = thread::spawn(move || {
            thread::sleep(Duration::from_millis(50));
            cancelling.cancel();
        });
        let result = supervise_child(
            (2, 2),
            &output,
            &log,
            &mut child,
            Backend::Cpu,
            Instant::now() + Duration::from_secs(2),
            &token,
        );
        canceller.join().unwrap();
        assert!(matches!(
            result,
            Err(AttemptError::App(AppError::Cancelled))
        ));
        assert!(child.try_wait().unwrap().is_some());
    }

    #[cfg(unix)]
    #[test]
    fn supervisor_times_out_and_reaps_an_active_worker() {
        let root = tempfile::tempdir().unwrap();
        let output = root.path().join("output.png");
        let log = root.path().join("worker.log");
        let mut child = logged_child("exec sleep 60", &log);
        let result = supervise_child(
            (2, 2),
            &output,
            &log,
            &mut child,
            Backend::Cpu,
            Instant::now() + Duration::from_millis(50),
            &CancellationToken::default(),
        );
        assert!(
            matches!(result, Err(AttemptError::App(AppError::BackgroundRemoval(message))) if message.contains("timed out"))
        );
        assert!(child.try_wait().unwrap().is_some());
    }

    #[cfg(unix)]
    #[test]
    fn failed_gpu_worker_is_distinguished_from_cpu_failure() {
        let root = tempfile::tempdir().unwrap();
        let output = root.path().join("output.png");
        let log = root.path().join("worker.log");
        let mut child = logged_child("echo model failed >&2; exit 9", &log);
        let result = supervise_child(
            (2, 2),
            &output,
            &log,
            &mut child,
            Backend::Gpu,
            Instant::now() + Duration::from_secs(1),
            &CancellationToken::default(),
        );
        assert!(
            matches!(result, Err(AttemptError::Gpu(AppError::BackgroundRemoval(message))) if message.contains("model failed"))
        );
    }

    #[cfg(unix)]
    #[test]
    fn supervisor_rejects_wrong_sized_output() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("wrong-sized.png");
        RgbaImage::from_pixel(1, 1, Rgba([0, 0, 0, 255]))
            .save(&source)
            .unwrap();
        let output = root.path().join("output.png");
        let log = root.path().join("worker.log");
        let log_file = File::create(&log).unwrap();
        let mut child = Command::new("sh")
            .args([
                "-c",
                "cp \"$1\" \"$2\"",
                "sh",
                source.to_str().unwrap(),
                output.to_str().unwrap(),
            ])
            .stdout(log_file.try_clone().unwrap())
            .stderr(log_file)
            .spawn()
            .unwrap();
        let result = supervise_child(
            (2, 2),
            &output,
            &log,
            &mut child,
            Backend::Cpu,
            Instant::now() + Duration::from_secs(1),
            &CancellationToken::default(),
        );
        assert!(matches!(
            result,
            Err(AttemptError::App(AppError::InvalidDimensions))
        ));
    }
}
