use std::cmp::Ordering;

use gio::prelude::*;

use crate::error::{AppError, Result};

const ATTRIBUTES: &str = "standard::name,standard::display-name,standard::type,standard::size,time::modified,time::created,time::access,metadata::nautilus-list-view-sort-column,metadata::nautilus-list-view-sort-reversed,metadata::nautilus-icon-view-sort-by,metadata::nautilus-icon-view-sort-reversed";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortOrder {
    Name,
    Modified,
    Created,
    Size,
    FileType,
    Accessed,
}

#[derive(Debug, Clone)]
struct Entry {
    file: gio::File,
    name: String,
    size: u64,
    modified: i64,
    created: i64,
    accessed: i64,
    file_type: String,
}

#[derive(Debug, Clone)]
pub struct DirectorySequence {
    entries: Vec<gio::File>,
    current: usize,
    pub order: SortOrder,
    pub reversed: bool,
}

impl DirectorySequence {
    pub fn build(current: &gio::File, fallback: SortOrder) -> Result<Self> {
        let parent = current
            .parent()
            .ok_or_else(|| AppError::FileMissing(current.uri().into()))?;
        let parent_info = parent
            .query_info(
                ATTRIBUTES,
                gio::FileQueryInfoFlags::NONE,
                gio::Cancellable::NONE,
            )
            .ok();
        let (order, reversed) = parent_info
            .as_ref()
            .and_then(nautilus_sort)
            .unwrap_or((fallback, false));

        let enumerator = parent
            .enumerate_children(
                ATTRIBUTES,
                gio::FileQueryInfoFlags::NONE,
                gio::Cancellable::NONE,
            )
            .map_err(|error| AppError::Io(std::io::Error::other(error)))?;
        let mut entries = Vec::new();
        while let Some(info) = enumerator
            .next_file(gio::Cancellable::NONE)
            .map_err(|error| AppError::Io(std::io::Error::other(error)))?
        {
            if info.file_type() != gio::FileType::Regular {
                continue;
            }
            let name = info.name();
            entries.push(Entry {
                file: parent.child(&name),
                name: info.display_name().to_string(),
                size: info.size().max(0) as u64,
                modified: info
                    .modification_date_time()
                    .map_or(0, |date| date.to_unix()),
                created: info.creation_date_time().map_or(0, |date| date.to_unix()),
                accessed: info.access_date_time().map_or(0, |date| date.to_unix()),
                file_type: extension(&info.display_name()).to_owned(),
            });
        }
        entries.sort_by(|left, right| compare_entry(left, right, order));
        if reversed {
            entries.reverse();
        }
        let entries: Vec<_> = entries
            .into_iter()
            .filter(|entry| is_supported(&entry.name))
            .map(|entry| entry.file)
            .collect();
        let current_uri = current.uri();
        let current_index = entries
            .iter()
            .position(|file| file.uri() == current_uri)
            .ok_or_else(|| AppError::FileMissing(current_uri.into()))?;
        Ok(Self {
            entries,
            current: current_index,
            order,
            reversed,
        })
    }

    pub fn current(&self) -> &gio::File {
        &self.entries[self.current]
    }

    pub fn previous(&mut self) -> Option<&gio::File> {
        if self.current == 0 {
            None
        } else {
            self.current -= 1;
            self.entries.get(self.current)
        }
    }

    pub fn next_image(&mut self) -> Option<&gio::File> {
        if self.current + 1 >= self.entries.len() {
            None
        } else {
            self.current += 1;
            self.entries.get(self.current)
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn neighbors(&self) -> Vec<gio::File> {
        let mut neighbors = Vec::with_capacity(2);
        if self.current > 0 {
            neighbors.push(self.entries[self.current - 1].clone());
        }
        if self.current + 1 < self.entries.len() {
            neighbors.push(self.entries[self.current + 1].clone());
        }
        neighbors
    }
}

pub fn is_supported(name: &str) -> bool {
    matches!(
        extension(name).as_str(),
        "png"
            | "apng"
            | "jpg"
            | "jpeg"
            | "gif"
            | "webp"
            | "avif"
            | "heif"
            | "heic"
            | "bmp"
            | "tif"
            | "tiff"
            | "svg"
            | "svgz"
            | "jp2"
            | "j2k"
            | "jxl"
            | "qoi"
            | "ico"
            | "exr"
            | "pbm"
            | "pgm"
            | "ppm"
            | "pnm"
            | "tga"
            | "xbm"
            | "xpm"
    )
}

fn nautilus_sort(info: &gio::FileInfo) -> Option<(SortOrder, bool)> {
    let column = info
        .attribute_string("metadata::nautilus-list-view-sort-column")
        .or_else(|| info.attribute_string("metadata::nautilus-icon-view-sort-by"))?;
    let order = match column.as_str() {
        "name" | "display_name" => SortOrder::Name,
        "mtime" | "date_modified" => SortOrder::Modified,
        "btime" | "date_created" => SortOrder::Created,
        "size" => SortOrder::Size,
        "type" | "detailed_type" => SortOrder::FileType,
        "atime" | "date_accessed" => SortOrder::Accessed,
        _ => return None,
    };
    let reversed = info.boolean("metadata::nautilus-list-view-sort-reversed")
        || info.boolean("metadata::nautilus-icon-view-sort-reversed");
    Some((order, reversed))
}

fn compare_entry(left: &Entry, right: &Entry, order: SortOrder) -> Ordering {
    let primary = match order {
        SortOrder::Name => natural_compare(&left.name, &right.name),
        SortOrder::Modified => left.modified.cmp(&right.modified),
        SortOrder::Created => left.created.cmp(&right.created),
        SortOrder::Size => left.size.cmp(&right.size),
        SortOrder::FileType => left.file_type.cmp(&right.file_type),
        SortOrder::Accessed => left.accessed.cmp(&right.accessed),
    };
    primary.then_with(|| natural_compare(&left.name, &right.name))
}

fn natural_compare(left: &str, right: &str) -> Ordering {
    let mut left = left.chars().peekable();
    let mut right = right.chars().peekable();
    loop {
        match (left.peek(), right.peek()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(a), Some(b)) if a.is_ascii_digit() && b.is_ascii_digit() => {
                let left_number = take_digits(&mut left);
                let right_number = take_digits(&mut right);
                let ordering = left_number
                    .trim_start_matches('0')
                    .len()
                    .cmp(&right_number.trim_start_matches('0').len())
                    .then_with(|| {
                        left_number
                            .trim_start_matches('0')
                            .cmp(right_number.trim_start_matches('0'))
                    })
                    .then_with(|| left_number.len().cmp(&right_number.len()));
                if ordering != Ordering::Equal {
                    return ordering;
                }
            }
            (Some(_), Some(_)) => {
                let left_char = left.next().unwrap_or_default();
                let right_char = right.next().unwrap_or_default();
                let ordering = left_char
                    .to_lowercase()
                    .to_string()
                    .cmp(&right_char.to_lowercase().to_string());
                if ordering != Ordering::Equal {
                    return ordering;
                }
            }
        }
    }
}

fn take_digits(iter: &mut std::iter::Peekable<std::str::Chars<'_>>) -> String {
    let mut digits = String::new();
    while iter.peek().is_some_and(char::is_ascii_digit) {
        if let Some(digit) = iter.next() {
            digits.push(digit);
        }
    }
    digits
}

fn extension(name: &str) -> String {
    name.rsplit_once('.')
        .map_or("", |(_, extension)| extension)
        .to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use std::cmp::Ordering;

    use super::{is_supported, natural_compare};

    #[test]
    fn compares_numbers_naturally() {
        assert_eq!(natural_compare("image2.png", "image10.png"), Ordering::Less);
    }

    #[test]
    fn supports_required_extensions_case_insensitively() {
        assert!(is_supported("photo.HEIC"));
        assert!(is_supported("vector.svgz"));
        assert!(!is_supported("video.mp4"));
    }
}
