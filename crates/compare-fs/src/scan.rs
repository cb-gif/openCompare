use crate::{
    check_cancel, files_equal, io_error, DirectoryEntryPair, DirectoryReport, EntryKind,
    EntryStatus, FileError,
};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
};
use walkdir::WalkDir;

const BATCH_SIZE: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirectoryPhase {
    Discovering,
    Comparing,
    Complete,
    Cancelled,
    Failed,
}

#[derive(Debug, Clone)]
pub struct DirectoryBatch {
    pub left: PathBuf,
    pub right: PathBuf,
    pub phase: DirectoryPhase,
    pub scanned: usize,
    /// Upserts by relative path, never evidence that the entire scan is complete.
    pub entries: Vec<DirectoryEntryPair>,
}

#[derive(Default)]
struct Pair {
    left: Option<Entry>,
    right: Option<Entry>,
}
struct Entry {
    kind: EntryKind,
    size: u64,
    error: Option<String>,
}

struct Reporter<F> {
    left: PathBuf,
    right: PathBuf,
    phase: DirectoryPhase,
    scanned: usize,
    pending: Vec<DirectoryEntryPair>,
    emit: F,
}
impl<F: FnMut(DirectoryBatch)> Reporter<F> {
    fn row(&mut self, row: DirectoryEntryPair) {
        self.pending.push(row);
        self.scanned += 1;
        if self.pending.len() >= BATCH_SIZE {
            self.flush();
        }
    }
    fn flush(&mut self) {
        (self.emit)(DirectoryBatch {
            left: self.left.clone(),
            right: self.right.clone(),
            phase: self.phase,
            scanned: self.scanned,
            entries: std::mem::take(&mut self.pending),
        });
    }
}

pub fn compare_directories_streaming(
    left: &Path,
    right: &Path,
    cancel: &AtomicBool,
    emit: impl FnMut(DirectoryBatch),
) -> Result<DirectoryReport, FileError> {
    check_cancel(cancel)?;
    let left = std::path::absolute(left).map_err(|e| io_error(left, e))?;
    let right = std::path::absolute(right).map_err(|e| io_error(right, e))?;
    for path in [&left, &right] {
        if !fs::metadata(path).map_err(|e| io_error(path, e))?.is_dir() {
            return Err(FileError::NotDirectory(path.clone()));
        }
    }
    let mut reporter = Reporter {
        left: left.clone(),
        right: right.clone(),
        phase: DirectoryPhase::Discovering,
        scanned: 0,
        pending: Vec::new(),
        emit,
    };
    reporter.flush();
    let mut pairs: BTreeMap<PathBuf, Pair> = BTreeMap::new();
    for (root, is_left) in [(&left, true), (&right, false)] {
        for entry in WalkDir::new(root).follow_links(false).min_depth(1) {
            check_cancel(cancel)?;
            let (path, item) = match entry {
                Ok(entry) => {
                    let file_type = entry.file_type();
                    let kind = if file_type.is_symlink() {
                        EntryKind::Symlink
                    } else if file_type.is_file() {
                        EntryKind::File
                    } else if file_type.is_dir() {
                        EntryKind::Directory
                    } else {
                        EntryKind::Other
                    };
                    let metadata = entry.metadata();
                    (
                        entry.path().to_owned(),
                        Entry {
                            kind,
                            size: metadata.as_ref().map_or(0, |m| m.len()),
                            error: metadata.err().map(|e| e.to_string()),
                        },
                    )
                }
                Err(error) => (
                    error.path().unwrap_or(root).to_owned(),
                    Entry {
                        kind: EntryKind::Other,
                        size: 0,
                        error: Some(error.to_string()),
                    },
                ),
            };
            let relative = path.strip_prefix(root).unwrap_or(&path).to_owned();
            let pair = pairs.entry(relative.clone()).or_default();
            if is_left {
                pair.left = Some(item);
            } else {
                pair.right = Some(item);
            }
            let mut row = make_row(relative, pair, EntryStatus::Pending);
            if row.error.is_some() {
                row.status = EntryStatus::Error;
            }
            reporter.row(row);
        }
        reporter.flush();
        check_cancel(cancel)?;
    }
    reporter.phase = DirectoryPhase::Comparing;
    reporter.flush();
    let mut entries = Vec::with_capacity(pairs.len());
    for (relative, pair) in pairs {
        check_cancel(cancel)?;
        let mut row = make_row(relative.clone(), &pair, EntryStatus::Pending);
        row.status = match (&pair.left, &pair.right) {
            (Some(l), Some(r)) if l.kind != r.kind => EntryStatus::TypeConflict,
            (Some(l), Some(_)) if l.kind == EntryKind::Directory => EntryStatus::Directory,
            (Some(l), Some(r)) if l.kind == EntryKind::File => {
                if l.size != r.size {
                    EntryStatus::Different
                } else {
                    match files_equal(&left.join(&relative), &right.join(&relative), cancel) {
                        Ok(true) => EntryStatus::Equal,
                        Ok(false) => EntryStatus::Different,
                        Err(FileError::Cancelled) => return Err(FileError::Cancelled),
                        Err(error) => {
                            row.error = Some(error.to_string());
                            EntryStatus::Error
                        }
                    }
                }
            }
            (Some(l), Some(_)) if l.kind == EntryKind::Symlink => {
                match (
                    fs::read_link(left.join(&relative)),
                    fs::read_link(right.join(&relative)),
                ) {
                    (Ok(l), Ok(r)) => {
                        if l == r {
                            EntryStatus::Equal
                        } else {
                            EntryStatus::Different
                        }
                    }
                    (Err(error), _) | (_, Err(error)) => {
                        row.error = Some(error.to_string());
                        EntryStatus::Error
                    }
                }
            }
            (Some(_), Some(_)) => {
                row.error = Some("Unsupported file type".into());
                EntryStatus::Error
            }
            (Some(_), None) => EntryStatus::LeftOnly,
            (None, Some(_)) => EntryStatus::RightOnly,
            (None, None) => unreachable!(),
        };
        if row.error.is_some() {
            row.status = EntryStatus::Error;
        }
        entries.push(row.clone());
        reporter.row(row);
    }
    reporter.flush();
    check_cancel(cancel)?;
    Ok(DirectoryReport {
        left,
        right,
        entries,
        complete: true,
    })
}

fn make_row(relative_path: PathBuf, pair: &Pair, status: EntryStatus) -> DirectoryEntryPair {
    DirectoryEntryPair {
        relative_path,
        status,
        left_kind: pair.left.as_ref().map(|e| e.kind),
        right_kind: pair.right.as_ref().map(|e| e.kind),
        left_size: pair.left.as_ref().map(|e| e.size),
        right_size: pair.right.as_ref().map(|e| e.size),
        error: pair
            .left
            .as_ref()
            .and_then(|e| e.error.clone())
            .or_else(|| pair.right.as_ref().and_then(|e| e.error.clone())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    #[test]
    fn discovery_is_pending_until_both_sides_and_contents_have_been_compared() {
        let left = tempfile::tempdir().unwrap();
        let right = tempfile::tempdir().unwrap();
        fs::write(left.path().join("sample"), "same").unwrap();
        fs::write(right.path().join("sample"), "same").unwrap();
        let mut batches = Vec::new();
        let report = compare_directories_streaming(
            left.path(),
            right.path(),
            &AtomicBool::new(false),
            |batch| batches.push(batch),
        )
        .unwrap();
        let discovered: Vec<_> = batches
            .iter()
            .filter(|b| b.phase == DirectoryPhase::Discovering)
            .flat_map(|b| &b.entries)
            .collect();
        assert!(!discovered.is_empty());
        assert!(discovered
            .iter()
            .all(|row| row.status == EntryStatus::Pending));
        assert!(batches
            .iter()
            .filter(|b| b.phase == DirectoryPhase::Comparing)
            .flat_map(|b| &b.entries)
            .any(|row| row.status == EntryStatus::Equal));
        assert!(report.complete);
        assert!(report
            .entries
            .iter()
            .all(|row| row.status != EntryStatus::Pending));
    }

    #[test]
    fn cancelled_discovery_keeps_observed_entries_pending_instead_of_equal() {
        let left = tempfile::tempdir().unwrap();
        let right = tempfile::tempdir().unwrap();
        fs::write(left.path().join("a"), "a").unwrap();
        let cancel = AtomicBool::new(false);
        let mut partial = Vec::new();
        let result = compare_directories_streaming(left.path(), right.path(), &cancel, |batch| {
            if !batch.entries.is_empty() {
                partial.extend(batch.entries);
                cancel.store(true, Ordering::Relaxed);
            }
        });
        assert!(matches!(result, Err(FileError::Cancelled)));
        assert_eq!(partial.len(), 1);
        assert_eq!(partial[0].status, EntryStatus::Pending);
    }
}
