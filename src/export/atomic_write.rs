use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

use crate::error::{AppError, Result};

pub fn atomic_save(
    destination: &Path,
    encode: impl FnOnce(&mut dyn Write) -> Result<()>,
) -> Result<()> {
    let parent = destination.parent().unwrap_or_else(|| Path::new("."));
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    {
        let mut writer = BufWriter::new(temporary.as_file_mut());
        encode(&mut writer)?;
        writer.flush()?;
    }
    temporary.as_file().sync_all()?;
    temporary
        .persist(destination)
        .map_err(|error| AppError::Io(error.error))?;
    if let Ok(directory) = File::open(parent) {
        let _ = directory.sync_all();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::atomic_save;
    use crate::error::AppError;

    #[test]
    fn failed_write_does_not_replace_destination() {
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("image.png");
        std::fs::write(&destination, b"original").unwrap();
        let result = atomic_save(&destination, |writer| {
            writer.write_all(b"partial")?;
            Err(AppError::Cancelled)
        });
        assert!(result.is_err());
        assert_eq!(std::fs::read(destination).unwrap(), b"original");
    }
}
