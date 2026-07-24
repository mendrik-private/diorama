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
            let name = info.name();
            let file = parent.child(&name);
            let is_regular = info.file_type() == gio::FileType::Regular
                || (info.file_type() == gio::FileType::SymbolicLink
                    && file.query_file_type(gio::FileQueryInfoFlags::NONE, gio::Cancellable::NONE)
                        == gio::FileType::Regular);
            if !is_regular {
                continue;
            }
            entries.push(Entry {
                file,
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
            .position(|file| file.equal(current))
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

    pub fn replacement_after_current_removed(&self) -> Option<gio::File> {
        self.replacements_after_current_removed().next()
    }

    pub fn replacements_after_current_removed(&self) -> impl Iterator<Item = gio::File> + '_ {
        self.entries[self.current + 1..]
            .iter()
            .chain(self.entries[..self.current].iter().rev())
            .cloned()
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

pub fn find_matching_file(reference: &gio::File, target: &gio::File) -> Result<Option<gio::File>> {
    let Some(target_name) = target.basename() else {
        return Ok(None);
    };
    let target_key = comparable_name(&target_name.to_string_lossy());
    if target_key.is_empty() {
        return Ok(None);
    }
    let parent = reference
        .parent()
        .ok_or_else(|| AppError::FileMissing(reference.uri().into()))?;
    let enumerator = parent
        .enumerate_children(
            ATTRIBUTES,
            gio::FileQueryInfoFlags::NONE,
            gio::Cancellable::NONE,
        )
        .map_err(|error| AppError::Io(std::io::Error::other(error)))?;
    while let Some(info) = enumerator
        .next_file(gio::Cancellable::NONE)
        .map_err(|error| AppError::Io(std::io::Error::other(error)))?
    {
        if info.file_type() == gio::FileType::Regular
            && is_supported(&info.display_name())
            && comparable_name(&info.display_name()) == target_key
        {
            return Ok(Some(parent.child(info.name())));
        }
    }
    Ok(None)
}

fn comparable_name(name: &str) -> String {
    let stem = name.rsplit_once('.').map_or(name, |(stem, _)| stem);
    stem.chars()
        .filter(|character| character.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
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
    let reversed = boolean_attribute(info, "metadata::nautilus-list-view-sort-reversed")
        || boolean_attribute(info, "metadata::nautilus-icon-view-sort-reversed");
    Some((order, reversed))
}

fn boolean_attribute(info: &gio::FileInfo, attribute: &str) -> bool {
    info.attribute_type(attribute) == gio::FileAttributeType::Boolean && info.boolean(attribute)
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

    use gio::prelude::*;

    use super::{
        DirectorySequence, SortOrder, boolean_attribute, comparable_name, find_matching_file,
        is_supported, natural_compare,
    };

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

    #[test]
    fn comparable_names_ignore_case_separators_and_extensions() {
        assert_eq!(
            comparable_name("Frame-001.PNG"),
            comparable_name("frame_001.webp")
        );
    }

    #[test]
    fn only_reads_boolean_file_attributes_as_booleans() {
        let info = gio::FileInfo::new();
        info.set_attribute_string("metadata::nautilus-list-view-sort-reversed", "true");
        info.set_attribute_boolean("metadata::nautilus-icon-view-sort-reversed", true);

        assert!(!boolean_attribute(
            &info,
            "metadata::nautilus-list-view-sort-reversed"
        ));
        assert!(boolean_attribute(
            &info,
            "metadata::nautilus-icon-view-sort-reversed"
        ));
        assert!(!boolean_attribute(&info, "metadata::missing"));
    }

    #[test]
    fn finds_a_matching_image_in_the_comparison_folder() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let reference = directory.path().join("reference.png");
        let counterpart = directory.path().join("frame_001.webp");
        let target = directory.path().join("Frame-001.PNG");
        std::fs::write(&reference, []).expect("reference file");
        std::fs::write(&counterpart, []).expect("comparison file");

        let found = find_matching_file(
            &gio::File::for_path(reference),
            &gio::File::for_path(target),
        )
        .expect("directory can be searched");

        assert_eq!(found.and_then(|file| file.path()), Some(counterpart));
    }

    #[test]
    fn replacement_after_removal_prefers_next_then_previous() {
        let directory = tempfile::tempdir().expect("temporary directory");
        for name in ["a.png", "b.png", "c.png"] {
            std::fs::write(directory.path().join(name), []).expect("image fixture");
        }

        let middle = DirectorySequence::build(
            &gio::File::for_path(directory.path().join("b.png")),
            SortOrder::Name,
        )
        .expect("middle sequence");
        let last = DirectorySequence::build(
            &gio::File::for_path(directory.path().join("c.png")),
            SortOrder::Name,
        )
        .expect("last sequence");
        let single_directory = tempfile::tempdir().expect("single-image directory");
        let single_path = single_directory.path().join("only.png");
        std::fs::write(&single_path, []).expect("single image fixture");
        let single = DirectorySequence::build(&gio::File::for_path(single_path), SortOrder::Name)
            .expect("single sequence");

        assert_eq!(
            middle
                .replacement_after_current_removed()
                .and_then(|file| file.basename()),
            Some("c.png".into())
        );
        assert_eq!(
            last.replacement_after_current_removed()
                .and_then(|file| file.basename()),
            Some("b.png".into())
        );
        assert!(single.replacement_after_current_removed().is_none());
    }

    #[test]
    fn replacement_candidates_continue_past_other_removed_files() {
        let directory = tempfile::tempdir().expect("temporary directory");
        for name in ["a.png", "b.png", "c.png", "d.png"] {
            std::fs::write(directory.path().join(name), []).expect("image fixture");
        }
        let sequence = DirectorySequence::build(
            &gio::File::for_path(directory.path().join("b.png")),
            SortOrder::Name,
        )
        .expect("directory sequence");

        let candidates = sequence
            .replacements_after_current_removed()
            .filter_map(|file| {
                file.basename()
                    .map(|name| name.to_string_lossy().into_owned())
            })
            .collect::<Vec<_>>();

        assert_eq!(candidates, ["c.png", "d.png", "a.png"]);
    }

    #[test]
    fn rebuilding_after_file_creation_and_deletion_updates_the_sequence() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let current = directory.path().join("b.png");
        std::fs::write(directory.path().join("a.png"), []).expect("first image");
        std::fs::write(&current, []).expect("current image");
        assert_eq!(
            DirectorySequence::build(&gio::File::for_path(&current), SortOrder::Name)
                .expect("initial sequence")
                .len(),
            2
        );

        std::fs::write(directory.path().join("c.png"), []).expect("new image");
        std::fs::remove_file(directory.path().join("a.png")).expect("remove old image");

        let rebuilt = DirectorySequence::build(&gio::File::for_path(current), SortOrder::Name)
            .expect("rebuilt sequence");
        assert_eq!(rebuilt.len(), 2);
        assert_eq!(
            rebuilt
                .replacement_after_current_removed()
                .and_then(|file| file.basename()),
            Some("c.png".into())
        );
    }

    #[cfg(unix)]
    #[test]
    fn regular_file_symlinks_are_navigable_but_broken_symlinks_are_ignored() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().expect("temporary directory");
        let source = directory.path().join("source.png");
        let current = directory.path().join("linked.png");
        std::fs::write(&source, []).expect("source image");
        symlink(&source, &current).expect("image symlink");
        symlink(
            directory.path().join("missing.png"),
            directory.path().join("broken.png"),
        )
        .expect("broken image symlink");

        let sequence = DirectorySequence::build(&gio::File::for_path(current), SortOrder::Name)
            .expect("symlink sequence");

        assert_eq!(sequence.len(), 2);
        assert_eq!(
            sequence.current().basename().as_deref(),
            Some(std::path::Path::new("linked.png"))
        );
    }
}
