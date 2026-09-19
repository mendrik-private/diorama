//! The optional Python distribution embedded by `build.rs`.

use std::{
    collections::HashSet,
    fs::{self, OpenOptions},
    io,
    path::{Component, Path, PathBuf},
};

use flate2::read::GzDecoder;
use sha2::{Digest, Sha256};
use tar::Archive;

use crate::{
    document::CancellationToken,
    error::{AppError, Result},
};

pub(super) struct EmbeddedRuntimeInfo {
    bytes: &'static [u8],
    sha256: &'static str,
    target: &'static str,
}

include!(concat!(env!("OUT_DIR"), "/embedded_python_runtime.rs"));

const ROOT: &str = "diorama-python-runtime";
const MARKER: &str = ".diorama-runtime-complete";
const SCHEMA: &str = "diorama-python-runtime-v1";

pub(super) struct MaterializedRuntime {
    pub(super) python: PathBuf,
    pub(super) worker: PathBuf,
}

pub(super) fn bundled_runtime_available() -> bool {
    EMBEDDED_RUNTIME.is_some()
}

pub(super) fn materialize(
    cache: &Path,
    cancellation: &CancellationToken,
) -> Result<MaterializedRuntime> {
    let info = EMBEDDED_RUNTIME.ok_or_else(|| {
        AppError::Inpainting("No Python runtime was embedded in this build".into())
    })?;
    materialize_info(cache, &info, cancellation)
}

fn materialize_info(
    cache: &Path,
    info: &EmbeddedRuntimeInfo,
    cancellation: &CancellationToken,
) -> Result<MaterializedRuntime> {
    let root = cache.join("diorama/python-runtime");
    fs::create_dir_all(&root)?;
    let destination = root.join(info.sha256);
    if destination.exists() {
        return verify_ready(&destination, info);
    }
    cancellation.check()?;
    if hex(&Sha256::digest(info.bytes)) != info.sha256 {
        return Err(AppError::Inpainting(
            "Embedded Python runtime checksum mismatch".into(),
        ));
    }
    let temporary = tempfile::Builder::new()
        .prefix(".unpack-")
        .tempdir_in(&root)?;
    extract(info, temporary.path(), cancellation)?;
    write_marker(temporary.path(), info)?;
    match fs::rename(temporary.path(), &destination) {
        Ok(()) => verify_ready(&destination, info),
        Err(_error) if destination.exists() => verify_ready(&destination, info),
        Err(error) => Err(error.into()),
    }
}

fn verify_ready(root: &Path, info: &EmbeddedRuntimeInfo) -> Result<MaterializedRuntime> {
    let expected = format!(
        "schema={SCHEMA}\nsha256={}\ntarget={}\n",
        info.sha256, info.target
    );
    if fs::read_to_string(root.join(MARKER)).ok().as_deref() != Some(&expected) {
        return Err(AppError::Inpainting(format!(
            "Python runtime cache {} is incomplete or corrupt; remove that one cache directory and retry",
            root.display()
        )));
    }
    let python = root.join("python/bin/python3");
    let worker = root.join("worker/lama_worker.py");
    if !python.is_file() || !worker.is_file() {
        return Err(AppError::Inpainting(
            "Python runtime cache is missing its interpreter or worker".into(),
        ));
    }
    #[cfg(unix)]
    if std::os::unix::fs::PermissionsExt::mode(&fs::metadata(&python)?.permissions()) & 0o111 == 0 {
        return Err(AppError::Inpainting(
            "Python runtime cache interpreter is not executable".into(),
        ));
    }
    Ok(MaterializedRuntime { python, worker })
}

fn write_marker(root: &Path, info: &EmbeddedRuntimeInfo) -> Result<()> {
    fs::write(
        root.join(MARKER),
        format!(
            "schema={SCHEMA}\nsha256={}\ntarget={}\n",
            info.sha256, info.target
        ),
    )?;
    Ok(())
}

fn extract(
    info: &EmbeddedRuntimeInfo,
    output: &Path,
    cancellation: &CancellationToken,
) -> Result<()> {
    let decoder = GzDecoder::new(info.bytes);
    let mut archive = Archive::new(decoder);
    let mut paths = HashSet::new();
    for item in archive
        .entries()
        .map_err(|error| AppError::Inpainting(format!("Invalid Python runtime archive: {error}")))?
    {
        cancellation.check()?;
        let mut entry = item.map_err(|error| {
            AppError::Inpainting(format!("Invalid Python runtime archive: {error}"))
        })?;
        let path = entry.path().map_err(|error| {
            AppError::Inpainting(format!("Invalid Python runtime archive path: {error}"))
        })?;
        let relative = safe_path(&path)?;
        if !paths.insert(relative.clone()) {
            return Err(AppError::Inpainting(format!(
                "Python runtime archive has duplicate path {}",
                relative.display()
            )));
        }
        let kind = entry.header().entry_type();
        let destination = output.join(&relative);
        if kind.is_dir() {
            fs::create_dir_all(&destination)?;
            set_mode(&destination, entry.header().mode().unwrap_or(0o755), true)?;
        } else if kind.is_file() {
            let parent = destination.parent().ok_or_else(|| {
                AppError::Inpainting("Python runtime archive has invalid root file".into())
            })?;
            fs::create_dir_all(parent)?;
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&destination)?;
            io::copy(&mut entry, &mut file)?;
            set_mode(&destination, entry.header().mode().unwrap_or(0o644), false)?;
        } else {
            return Err(AppError::Inpainting(format!(
                "Python runtime archive contains unsupported entry {}",
                relative.display()
            )));
        }
    }
    let mut decoder = archive.into_inner();
    io::copy(&mut decoder, &mut io::sink()).map_err(|error| {
        AppError::Inpainting(format!("Invalid Python runtime gzip stream: {error}"))
    })?;
    verify_unpacked(output)
}

fn safe_path(path: &Path) -> Result<PathBuf> {
    let mut parts = path.components();
    if parts.next() != Some(Component::Normal(ROOT.as_ref())) {
        return Err(AppError::Inpainting(
            "Python runtime archive entry is outside its root".into(),
        ));
    }
    let relative: PathBuf = parts.collect();
    if relative.as_os_str().is_empty()
        || relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(AppError::Inpainting(
            "Python runtime archive contains an unsafe path".into(),
        ));
    }
    Ok(relative)
}

fn verify_unpacked(root: &Path) -> Result<()> {
    let manifest = fs::read_to_string(root.join("manifest"))?;
    if !manifest
        .lines()
        .any(|line| line == format!("schema={SCHEMA}"))
    {
        return Err(AppError::Inpainting(
            "Python runtime archive has an unsupported manifest".into(),
        ));
    }
    let worker = root.join("worker/lama_worker.py");
    if !worker.is_file() || !root.join("python/bin/python3").is_file() {
        return Err(AppError::Inpainting(
            "Python runtime archive is missing its interpreter or worker".into(),
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32, _directory: bool) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode & 0o777))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: u32, _directory: bool) -> Result<()> {
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    fn archive() -> Vec<u8> {
        let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        {
            let mut tar = tar::Builder::new(&mut gzip);
            for (path, contents, mode) in [
                ("diorama-python-runtime/manifest", b"schema=diorama-python-runtime-v1\ntarget=fixture\npython=python/bin/python3\n".as_slice(), 0o644),
                ("diorama-python-runtime/python/bin/python3", b"fixture interpreter".as_slice(), 0o700),
                ("diorama-python-runtime/worker/lama_worker.py", b"fixture worker".as_slice(), 0o644),
            ] {
                let mut header = tar::Header::new_gnu();
                header.set_size(contents.len() as u64);
                header.set_mode(mode);
                header.set_cksum();
                tar.append_data(&mut header, path, contents).unwrap();
            }
            tar.finish().unwrap();
        }
        gzip.finish().unwrap()
    }

    #[test]
    fn extraction_restores_executable_files_and_rejects_unsafe_paths() {
        let bytes = Box::leak(archive().into_boxed_slice());
        let info = EmbeddedRuntimeInfo {
            bytes,
            sha256: "fixture",
            target: "fixture",
        };
        let directory = tempfile::tempdir().unwrap();
        extract(&info, directory.path(), &CancellationToken::default()).unwrap();
        let python = directory.path().join("python/bin/python3");
        assert_eq!(fs::read(&python).unwrap(), b"fixture interpreter");
        #[cfg(unix)]
        assert_ne!(
            std::os::unix::fs::PermissionsExt::mode(&fs::metadata(python).unwrap().permissions())
                & 0o111,
            0
        );
        assert!(safe_path(Path::new("diorama-python-runtime/../escape")).is_err());
        assert!(safe_path(Path::new("/diorama-python-runtime/worker")).is_err());
    }

    #[test]
    fn corrupt_gzip_is_rejected() {
        let bytes = b"not a gzip archive";
        let info = EmbeddedRuntimeInfo {
            bytes,
            sha256: "fixture",
            target: "fixture",
        };
        assert!(
            extract(
                &info,
                tempfile::tempdir().unwrap().path(),
                &CancellationToken::default()
            )
            .is_err()
        );
    }

    #[test]
    fn truncated_valid_gzip_and_duplicate_entries_are_rejected() {
        let mut truncated = archive();
        truncated.pop();
        let info = EmbeddedRuntimeInfo {
            bytes: Box::leak(truncated.into_boxed_slice()),
            sha256: "fixture",
            target: "fixture",
        };
        assert!(
            extract(
                &info,
                tempfile::tempdir().unwrap().path(),
                &CancellationToken::default()
            )
            .is_err()
        );

        let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        {
            let mut tar = tar::Builder::new(&mut gzip);
            for _ in 0..2 {
                let mut header = tar::Header::new_gnu();
                header.set_size(1);
                header.set_mode(0o644);
                header.set_cksum();
                tar.append_data(
                    &mut header,
                    "diorama-python-runtime/duplicate",
                    b"x".as_slice(),
                )
                .unwrap();
            }
            tar.finish().unwrap();
        }
        let duplicate = EmbeddedRuntimeInfo {
            bytes: Box::leak(gzip.finish().unwrap().into_boxed_slice()),
            sha256: "fixture",
            target: "fixture",
        };
        let error = extract(
            &duplicate,
            tempfile::tempdir().unwrap().path(),
            &CancellationToken::default(),
        )
        .unwrap_err();
        assert!(error.to_string().contains("duplicate path"));
    }

    #[test]
    fn link_entries_are_rejected_before_any_cache_is_published() {
        let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        {
            let mut tar = tar::Builder::new(&mut gzip);
            let mut link = tar::Header::new_gnu();
            link.set_entry_type(tar::EntryType::Symlink);
            link.set_size(0);
            link.set_mode(0o777);
            link.set_link_name("/etc/passwd").unwrap();
            link.set_cksum();
            tar.append_data(&mut link, "diorama-python-runtime/unsafe", io::empty())
                .unwrap();
            tar.finish().unwrap();
        }
        let bytes = Box::leak(gzip.finish().unwrap().into_boxed_slice());
        let info = EmbeddedRuntimeInfo {
            bytes,
            sha256: "fixture",
            target: "fixture",
        };
        let error = extract(
            &info,
            tempfile::tempdir().unwrap().path(),
            &CancellationToken::default(),
        )
        .unwrap_err();
        assert!(error.to_string().contains("unsupported entry"));
    }

    #[test]
    fn incomplete_cache_is_not_accepted_and_owner_execute_mode_is_valid() {
        let root = tempfile::tempdir().unwrap();
        let info = EmbeddedRuntimeInfo {
            bytes: b"fixture",
            sha256: "fixture",
            target: "fixture",
        };
        assert!(verify_ready(root.path(), &info).is_err());
        fs::create_dir_all(root.path().join("python/bin")).unwrap();
        fs::create_dir_all(root.path().join("worker")).unwrap();
        fs::write(root.path().join("python/bin/python3"), b"fixture").unwrap();
        fs::write(root.path().join("worker/lama_worker.py"), b"fixture").unwrap();
        #[cfg(unix)]
        fs::set_permissions(
            root.path().join("python/bin/python3"),
            std::fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        write_marker(root.path(), &info).unwrap();
        assert!(verify_ready(root.path(), &info).is_ok());
    }

    #[test]
    fn cache_reuses_a_valid_runtime() {
        let bytes = Box::leak(archive().into_boxed_slice());
        let digest = hex(&Sha256::digest(&*bytes));
        let info = EmbeddedRuntimeInfo {
            bytes,
            sha256: Box::leak(digest.into_boxed_str()),
            target: "fixture",
        };
        let cache = tempfile::tempdir().unwrap();
        let first = materialize_info(cache.path(), &info, &CancellationToken::default()).unwrap();
        let second = materialize_info(cache.path(), &info, &CancellationToken::default()).unwrap();
        assert_eq!(first.python, second.python);
    }

    #[test]
    fn concurrent_cache_materialization_publishes_one_complete_runtime() {
        let bytes = Box::leak(archive().into_boxed_slice());
        let digest = Box::leak(hex(&Sha256::digest(&*bytes)).into_boxed_str());
        let info = EmbeddedRuntimeInfo {
            bytes,
            sha256: digest,
            target: "fixture",
        };
        let cache = tempfile::tempdir().unwrap();
        let barrier = std::sync::Barrier::new(4);
        std::thread::scope(|scope| {
            let mut workers = Vec::new();
            for _ in 0..4 {
                workers.push(scope.spawn(|| {
                    barrier.wait();
                    materialize_info(cache.path(), &info, &CancellationToken::default())
                }));
            }
            for worker in workers {
                assert!(worker.join().unwrap().is_ok());
            }
        });
        let entries = fs::read_dir(cache.path().join("diorama/python-runtime"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>();
        assert_eq!(entries.len(), 1);
        assert!(!entries[0].to_string_lossy().starts_with(".unpack-"));
    }

    #[cfg(unix)]
    #[test]
    fn bundled_fixture_runs_after_embedding_when_available() {
        let Some(info) = EMBEDDED_RUNTIME else { return };
        let cache = tempfile::tempdir().unwrap();
        let runtime = materialize_info(cache.path(), &info, &CancellationToken::default()).unwrap();
        let output = std::process::Command::new(runtime.python)
            .arg("-I")
            .arg("-c")
            .arg("print('bundled-runtime')")
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, b"bundled-runtime\n");
        assert_eq!(
            fs::read(runtime.worker).unwrap(),
            include_bytes!("lama_worker.py")
        );
    }
}
