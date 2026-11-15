//! First-use provisioning, separate from the offline inference worker.
use super::*;
use std::sync::{Mutex, TryLockError};

const SCRIPT: &str = include_str!("../../../build-aux/setup-line-art.py");
// Model downloads can take much longer than inference on slow connections.
const TIMEOUT: Duration = Duration::from_secs(2 * 60 * 60);
// Serialize setup across windows without occupying the GPU inference gate
// during a potentially long download.
static PROVISIONING: Mutex<()> = Mutex::new(());

pub(super) fn prepare(
    runtime: &Runtime,
    cache: &Path,
    cancellation: &CancellationToken,
    progress: &dyn Fn(Progress),
) -> Result<Runtime> {
    prepare_with_config(
        runtime,
        cache,
        cancellation,
        progress,
        runtime
            .launch
            .runtime_config_path(RUNTIME_CONFIG)
            .as_deref(),
    )
}

fn prepare_with_config(
    runtime: &Runtime,
    cache: &Path,
    cancellation: &CancellationToken,
    progress: &dyn Fn(Progress),
    config: Option<&Path>,
) -> Result<Runtime> {
    cancellation.check()?;
    let mut prepared = runtime.clone();
    if runtime.bootstrap_venv.is_none()
        && verify_model(&runtime.model).is_ok()
        && verify_gguf(&runtime.gguf).is_ok()
    {
        return Ok(prepared);
    }
    progress(downloading());
    let _permit = loop {
        cancellation.check()?;
        match PROVISIONING.try_lock() {
            Ok(permit) => break permit,
            Err(TryLockError::Poisoned(error)) => break error.into_inner(),
            Err(TryLockError::WouldBlock) => std::thread::sleep(POLL_INTERVAL),
        }
    };
    // A different window may have completed setup while this one waited.
    if runtime.bootstrap_venv.is_none()
        && verify_model(&runtime.model).is_ok()
        && verify_gguf(&runtime.gguf).is_ok()
    {
        return Ok(prepared);
    }
    fs::create_dir_all(cache)?;
    // Flatpak's /tmp is private; the app cache is visible to host workers.
    let directory = tempfile::Builder::new()
        .prefix(".setup-")
        .tempdir_in(cache)?;
    let script = directory.path().join("setup-line-art.py");
    let log = directory.path().join("setup.log");
    fs::write(&script, SCRIPT)?;
    if let Some(venv) = &runtime.bootstrap_venv {
        let config = config.ok_or_else(|| {
            AppError::SketchGeneration(
                "HOME is unavailable; cannot record the line-art runtime".into(),
            )
        })?;
        let mut command = runtime
            .launch
            .command(&runtime.python, runtime.host_library_path.as_deref());
        command
            .arg(&script)
            .arg("--environment-only")
            .arg("--venv")
            .arg(venv)
            .arg("--runtime-config")
            .arg(config);
        run(&mut command, &log, cancellation, progress)?;
        let configuration = RuntimeConfiguration::read(Some(config));
        prepared.python = configuration
            .python
            .filter(|python| python.is_file())
            .ok_or_else(|| {
                AppError::SketchGeneration(
                    "Line-art setup did not record a usable Python interpreter".into(),
                )
            })?;
        prepared.host_library_path = configuration.library_path;
        prepared.bootstrap_venv = None;
    }
    let mut command = prepared
        .launch
        .command(&prepared.python, prepared.host_library_path.as_deref());
    command
        .arg(&script)
        .arg("--models-only")
        .arg("--model")
        .arg(&prepared.model)
        .arg("--gguf")
        .arg(&prepared.gguf);
    run(&mut command, &log, cancellation, progress)?;
    // Setup success alone is insufficient: retain the inference invariants.
    verify_model(&prepared.model)?;
    verify_gguf(&prepared.gguf)?;
    Ok(prepared)
}

fn downloading() -> Progress {
    Progress {
        fraction: 0.,
        remaining: Duration::ZERO,
        stage: Stage::Downloading,
    }
}

fn run(
    command: &mut Command,
    log: &Path,
    cancellation: &CancellationToken,
    progress: &dyn Fn(Progress),
) -> Result<()> {
    cancellation.check()?;
    let mut child = worker_process::spawn_logged(command, log).map_err(|error| {
        AppError::SketchGeneration(format!("Could not start automatic line-art setup: {error}"))
    })?;
    let status =
        worker_process::wait_with(&mut child, cancellation, TIMEOUT, POLL_INTERVAL, || {
            progress(downloading());
        })
        .map_err(|error| {
            error.into_app_error(AppError::SketchGeneration(
                "Downloading line-art dependencies timed out. Retry to resume completed files"
                    .into(),
            ))
        })?;
    cancellation.check()?;
    if !status.success() {
        return Err(AppError::SketchGeneration(format!(
            "Automatic line-art setup failed ({status}): {}",
            worker_process::log_tail(log)
        )));
    }
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn runtime(root: &Path, script: &str) -> Runtime {
        let python = root.join("python");
        worker_process::write_test_executable(&python, script);
        Runtime {
            python,
            bootstrap_venv: None,
            model: root.join("pipeline with spaces"),
            gguf: root.join("gguf with spaces").join(GGUF_FILE),
            device: None,
            launch: Launch::Direct,
            host_library_path: None,
            timeout: super::super::TIMEOUT,
            cancel_grace: resident::CANCEL_GRACE,
            idle_timeout: resident::IDLE_TIMEOUT,
            inference: std::sync::Arc::default(),
        }
    }

    #[test]
    fn setup_failure_reports_the_actionable_error_and_rejects_false_success() {
        let root = tempfile::tempdir().unwrap();
        for (script, expected) in [
            (
                "#!/bin/sh\necho 'Accept the licence at huggingface.co and authenticate' >&2\nexit 4\n",
                "Accept the licence",
            ),
            ("#!/bin/sh\nexit 0\n", "pipeline is missing"),
        ] {
            let runtime = runtime(root.path(), script);
            let error = prepare(
                &runtime,
                &root.path().join("cache"),
                &CancellationToken::default(),
                &|p| {
                    assert_eq!(p.stage, Stage::Downloading);
                },
            )
            .err()
            .expect("setup must fail")
            .to_string();
            assert!(error.contains(expected), "{error}");
            assert!(!runtime.model.join(REVISION_MARKER).exists());
        }
    }

    #[test]
    fn cancelling_setup_stops_the_process_and_does_not_mark_files_complete() {
        let root = tempfile::tempdir().unwrap();
        let runtime = runtime(
            root.path(),
            "#!/bin/sh\ntouch \"$(dirname \"$0\")/started\"\nexec sleep 30\n",
        );
        let cancellation = CancellationToken::default();
        let started = Instant::now();
        let result = prepare(&runtime, &root.path().join("cache"), &cancellation, &|_| {
            if root.path().join("started").exists() {
                cancellation.cancel();
            }
        });
        assert!(matches!(result, Err(AppError::Cancelled)));
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(!runtime.model.join(REVISION_MARKER).exists());
    }

    #[test]
    fn flatpak_bootstraps_the_runtime_then_downloads_to_explicit_paths() {
        let root = tempfile::tempdir().unwrap();
        let script = format!(
            r#"#!/bin/bash
shift # embedded script
mode="$1"; shift
while [ $# -gt 0 ]; do
  case "$1" in
    --venv) venv="$2" ;;
    --runtime-config) config="$2" ;;
    --model) model="$2" ;;
    --gguf) gguf="$2" ;;
  esac
  shift 2
done
if [ "$mode" = --environment-only ]; then
  mkdir -p "$venv/bin"
  cp "$0" "$venv/bin/python"
  printf 'python=%s\n' "$venv/bin/python" > "$config"
else
  mkdir -p "$model" "$(dirname "$gguf")"
  echo '{{}}' > "$model/model_index.json"
  echo '{revision}' > "$model/{REVISION_MARKER}"
  truncate -s {GGUF_SIZE} "$gguf"
  echo '{hash}' > "$(dirname "$gguf")/{GGUF_MARKER}"
fi
"#,
            revision = RECIPE.revision,
            hash = RECIPE.gguf_sha256
        );
        let mut runtime = runtime(root.path(), &script);
        let venv = root.path().join("venv with spaces");
        runtime.bootstrap_venv = Some(venv.clone());
        let launcher = root.path().join("flatpak-spawn");
        worker_process::write_test_executable(
            &launcher,
            "#!/bin/sh\nprintf '%s\\n' \"$@\" >> \"$(dirname \"$0\")/host-launches\"\nwhile [ \"${1#--}\" != \"$1\" ]; do shift; done\nexec \"$@\"\n",
        );
        runtime.launch = Launch::FlatpakHost { launcher };
        let config = root.path().join("runtime.conf");
        let prepared = prepare_with_config(
            &runtime,
            &root.path().join("cache with spaces"),
            &CancellationToken::default(),
            &|_| {},
            Some(&config),
        )
        .unwrap();
        assert_eq!(prepared.python, venv.join("bin/python"));
        assert!(prepared.bootstrap_venv.is_none());
        verify_model(&prepared.model).unwrap();
        verify_gguf(&prepared.gguf).unwrap();
        let launches = fs::read_to_string(root.path().join("host-launches")).unwrap();
        assert_eq!(launches.matches("--host\n").count(), 2);
        assert!(launches.contains("--environment-only\n"));
        assert!(launches.contains("--models-only\n"));
        assert!(launches.contains(runtime.model.to_str().unwrap()));
        assert!(launches.contains(runtime.gguf.to_str().unwrap()));
    }
}
