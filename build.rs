use std::{env, fs, io::Read, path::Path};

use flate2::read::GzDecoder;
use sha2::{Digest, Sha256};
use tar::Archive;

const SCHEMA: &str = "diorama-python-runtime-v1";

fn main() {
    println!("cargo:rerun-if-env-changed=DIORAMA_PYTHON_RUNTIME");
    println!("cargo:rerun-if-changed=src/tools/lama_worker.py");
    let output_directory = env::var("OUT_DIR").expect("Cargo always sets OUT_DIR");
    let output = Path::new(&output_directory);
    let generated = output.join("embedded_python_runtime.rs");
    let Some(archive_path) = env::var_os("DIORAMA_PYTHON_RUNTIME") else {
        fs::write(
            generated,
            "pub const EMBEDDED_RUNTIME: Option<EmbeddedRuntimeInfo> = None;\n",
        )
        .expect("write generated runtime definition");
        return;
    };
    let archive_path = Path::new(&archive_path);
    println!("cargo:rerun-if-changed={}", archive_path.display());
    let archive = fs::read(archive_path).unwrap_or_else(|error| {
        panic!(
            "DIORAMA_PYTHON_RUNTIME={} cannot be read: {error}",
            archive_path.display()
        )
    });
    let target = env::var("TARGET").expect("Cargo always sets TARGET");
    validate_archive(&archive, &target).unwrap_or_else(|error| {
        panic!(
            "DIORAMA_PYTHON_RUNTIME={} is not a compatible Diorama Python runtime: {error}",
            archive_path.display()
        )
    });
    fs::write(output.join("diorama-python-runtime.tar.gz"), &archive)
        .expect("copy bundled runtime to OUT_DIR");
    let digest = hex(&Sha256::digest(&archive));
    fs::write(
        generated,
        format!(
            "pub const EMBEDDED_RUNTIME: Option<EmbeddedRuntimeInfo> = Some(EmbeddedRuntimeInfo {{ bytes: include_bytes!(concat!(env!(\"OUT_DIR\"), \"/diorama-python-runtime.tar.gz\")), sha256: \"{digest}\", target: \"{target}\" }});\n"
        ),
    )
    .expect("write generated runtime definition");
}

fn validate_archive(bytes: &[u8], target: &str) -> Result<(), String> {
    let decoder = GzDecoder::new(bytes);
    let mut archive = Archive::new(decoder);
    let mut manifest = None;
    let mut worker = None;
    let mut interpreter_mode = None;
    for entry in archive.entries().map_err(|e| e.to_string())? {
        let mut entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path().map_err(|e| e.to_string())?;
        if entry.header().entry_type().is_file() {
            if path == Path::new("diorama-python-runtime/manifest") {
                let mut value = String::new();
                entry
                    .read_to_string(&mut value)
                    .map_err(|e| e.to_string())?;
                manifest = Some(value);
            } else if path == Path::new("diorama-python-runtime/worker/lama_worker.py") {
                let mut value = Vec::new();
                entry.read_to_end(&mut value).map_err(|e| e.to_string())?;
                worker = Some(value);
            } else if path == Path::new("diorama-python-runtime/python/bin/python3") {
                interpreter_mode = Some(entry.header().mode().map_err(|e| e.to_string())?);
            }
        }
    }
    let mut decoder = archive.into_inner();
    let mut tail = Vec::new();
    decoder
        .read_to_end(&mut tail)
        .map_err(|e| format!("invalid gzip stream: {e}"))?;
    let manifest = manifest.ok_or("missing diorama-python-runtime/manifest")?;
    let fields = manifest_fields(&manifest);
    if fields.get("schema") != Some(&SCHEMA) {
        return Err("unsupported or missing runtime schema".into());
    }
    if fields.get("target") != Some(&target) {
        return Err(format!(
            "archive target {:?} does not match Cargo target {target}",
            fields.get("target")
        ));
    }
    if fields.get("python") != Some(&"python/bin/python3") {
        return Err("manifest does not name python/bin/python3 as its interpreter".into());
    }
    if interpreter_mode.is_none_or(|mode| mode & 0o111 == 0) {
        return Err("missing executable regular python/bin/python3".into());
    }
    let worker = worker.ok_or("missing packaged worker")?;
    let source = fs::read("src/tools/lama_worker.py").map_err(|e| e.to_string())?;
    if worker != source {
        return Err("packaged worker differs from src/tools/lama_worker.py; run package-python-runtime.py again".into());
    }
    Ok(())
}

fn manifest_fields(manifest: &str) -> std::collections::BTreeMap<&str, &str> {
    manifest
        .lines()
        .filter_map(|line| line.split_once('='))
        .collect()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
