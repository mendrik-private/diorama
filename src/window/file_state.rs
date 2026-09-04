use std::time::SystemTime;

use gio::prelude::FileExt;

#[derive(Default)]
pub(super) struct PendingDirectoryChanges {
    pub(super) refresh_navigation: bool,
    pub(super) current_changed: bool,
    pub(super) current_removed: bool,
    pub(super) current_renamed_to: Option<gio::File>,
}

pub(super) fn merge_directory_change(
    pending: &mut PendingDirectoryChanges,
    current: &gio::File,
    file: &gio::File,
    other_file: Option<&gio::File>,
    event: gio::FileMonitorEvent,
) {
    let source_is_current = file.equal(current);
    let source_is_parent = current.parent().is_some_and(|parent| file.equal(&parent));
    let target_is_current = other_file.is_some_and(|target| target.equal(current));
    pending.refresh_navigation = true;
    match event {
        gio::FileMonitorEvent::Changed | gio::FileMonitorEvent::ChangesDoneHint => {
            pending.current_changed |= source_is_current;
        }
        gio::FileMonitorEvent::AttributeChanged => {}
        gio::FileMonitorEvent::Created | gio::FileMonitorEvent::MovedIn => {
            pending.current_changed |= source_is_current || target_is_current;
        }
        gio::FileMonitorEvent::Deleted => {
            pending.current_removed |= source_is_current || source_is_parent;
        }
        gio::FileMonitorEvent::MovedOut => {
            if source_is_current && let Some(target) = other_file {
                pending.current_renamed_to = Some(target.clone());
            }
            pending.current_removed |= source_is_current || source_is_parent;
        }
        gio::FileMonitorEvent::Moved | gio::FileMonitorEvent::Renamed => {
            if source_is_current {
                pending.current_renamed_to = other_file.cloned();
                pending.current_removed = true;
            } else if source_is_parent {
                pending.current_renamed_to = other_file.and_then(|target_parent| {
                    current
                        .basename()
                        .map(|basename| target_parent.child(basename))
                });
                pending.current_removed = true;
            }
            pending.current_changed |= target_is_current;
        }
        gio::FileMonitorEvent::PreUnmount | gio::FileMonitorEvent::Unmounted => {
            pending.current_removed = true;
        }
        _ => {}
    }
}

pub(super) fn files_equal(left: &Option<gio::File>, right: &Option<gio::File>) -> bool {
    match (left, right) {
        (Some(left), Some(right)) => left.equal(right),
        (None, None) => true,
        _ => false,
    }
}

pub(super) fn is_regular_file(file: &gio::File) -> bool {
    file.query_file_type(gio::FileQueryInfoFlags::NONE, gio::Cancellable::NONE)
        == gio::FileType::Regular
}

pub(super) fn is_directory(file: &gio::File) -> bool {
    file.query_file_type(gio::FileQueryInfoFlags::NONE, gio::Cancellable::NONE)
        == gio::FileType::Directory
}

pub(super) fn first_existing_folder(
    candidates: impl IntoIterator<Item = gio::File>,
) -> Option<gio::File> {
    candidates.into_iter().find(is_directory)
}

pub(super) fn source_revision_changed(
    previous: Option<SystemTime>,
    current: Option<SystemTime>,
    is_local: bool,
) -> bool {
    if is_local { previous != current } else { true }
}

pub(super) fn export_context_matches(
    current_load_generation: u64,
    exported_load_generation: u64,
    current_file: &Option<gio::File>,
    exported_file: &Option<gio::File>,
) -> bool {
    current_load_generation == exported_load_generation && files_equal(current_file, exported_file)
}
