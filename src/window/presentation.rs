use std::time::SystemTime;

use gio::prelude::FileExt;

pub(super) fn folder_path(file: &gio::File) -> String {
    let Some(folder) = file.parent() else {
        return file.uri().to_string();
    };
    folder.path().map_or_else(
        || folder.uri().to_string(),
        |path| path.display().to_string(),
    )
}

pub(super) fn compare_metadata(file: &gio::File, width: u32, height: u32) -> String {
    format!("{} · {width} × {height}", folder_path(file))
}

pub(super) fn relative_modified_time(modified: SystemTime, now: SystemTime) -> String {
    let (elapsed, is_past) = match now.duration_since(modified) {
        Ok(elapsed) => (elapsed, true),
        Err(error) => (error.duration(), false),
    };
    let seconds = elapsed.as_secs();
    if seconds < 60 {
        return "just now".to_owned();
    }

    let (value, unit) = if seconds < 60 * 60 {
        (seconds / 60, "minute")
    } else if seconds < 24 * 60 * 60 {
        (seconds / (60 * 60), "hour")
    } else if seconds < 30 * 24 * 60 * 60 {
        (seconds / (24 * 60 * 60), "day")
    } else if seconds < 365 * 24 * 60 * 60 {
        (seconds / (30 * 24 * 60 * 60), "month")
    } else {
        (seconds / (365 * 24 * 60 * 60), "year")
    };
    let unit = if value == 1 {
        unit.to_owned()
    } else {
        format!("{unit}s")
    };
    if is_past {
        format!("{value} {unit} ago")
    } else {
        format!("in {value} {unit}")
    }
}

pub(super) fn image_subtitle(
    folder: &str,
    dimensions: (u32, u32),
    zoom: f64,
    modified: Option<SystemTime>,
    now: SystemTime,
) -> String {
    let details = format!(
        "{folder} · {} × {} · {:.0}%",
        dimensions.0,
        dimensions.1,
        zoom * 100.0
    );
    match modified {
        Some(modified) => format!("{details} · {}", relative_modified_time(modified, now)),
        None => details,
    }
}
