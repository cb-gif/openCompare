use crate::{check_cancel, io_error, FileError};
use std::{
    collections::BTreeSet,
    fs::{self, File, Metadata},
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    time::SystemTime,
};

#[derive(Debug, Clone, PartialEq, Eq)]
struct Fingerprint {
    length: u64,
    modified: Option<SystemTime>,
    digest: blake3::Hash,
}

#[derive(Debug, Clone)]
pub struct CopyOperation {
    pub relative_path: PathBuf,
    pub bytes: u64,
    pub overwrites: bool,
    source: Fingerprint,
    destination: Option<Fingerprint>,
}

#[derive(Debug, Clone)]
pub struct CopyPlan {
    pub source_root: PathBuf,
    pub destination_root: PathBuf,
    pub operations: Vec<CopyOperation>,
}

impl CopyPlan {
    pub fn total_bytes(&self) -> u64 {
        self.operations.iter().map(|op| op.bytes).sum()
    }
    pub fn overwrite_count(&self) -> usize {
        self.operations.iter().filter(|op| op.overwrites).count()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CopyItemStatus {
    Copied,
    Failed(String),
    Cancelled,
    NotRun,
}

#[derive(Debug, Clone)]
pub struct CopyItemResult {
    pub relative_path: PathBuf,
    pub status: CopyItemStatus,
}

#[derive(Debug, Clone)]
pub struct CopyReport {
    pub source_root: PathBuf,
    pub destination_root: PathBuf,
    pub items: Vec<CopyItemResult>,
    pub cancelled: bool,
}

impl CopyReport {
    pub fn copied_count(&self) -> usize {
        self.items
            .iter()
            .filter(|item| item.status == CopyItemStatus::Copied)
            .count()
    }
    pub fn failed_count(&self) -> usize {
        self.items
            .iter()
            .filter(|item| matches!(item.status, CopyItemStatus::Failed(_)))
            .count()
    }
}

/// Freeze byte fingerprints for a concrete preview. No destination files are written.
pub fn prepare_copy(
    source_root: &Path,
    destination_root: &Path,
    paths: &[PathBuf],
    cancel: &AtomicBool,
    mut progress: impl FnMut(usize),
) -> Result<CopyPlan, FileError> {
    check_cancel(cancel)?;
    let source_root = canonical_root(source_root)?;
    let destination_root = canonical_root(destination_root)?;
    if source_root == destination_root {
        return Err(FileError::SameCopyPath(source_root));
    }
    let paths: BTreeSet<_> = paths.iter().cloned().collect();
    if paths.is_empty() {
        return Err(FileError::EmptyCopyPlan);
    }
    let mut operations = Vec::with_capacity(paths.len());
    for relative_path in paths {
        check_cancel(cancel)?;
        let source_path = checked_path(&source_root, &relative_path, false)?;
        let destination_path = checked_path(&destination_root, &relative_path, true)?;
        let source = fingerprint(&source_path, cancel)?;
        let destination = optional_fingerprint(&destination_path, cancel)?;
        if destination.is_some() {
            ensure_writable(&destination_path)?;
        }
        operations.push(CopyOperation {
            relative_path,
            bytes: source.length,
            overwrites: destination.is_some(),
            source,
            destination,
        });
        progress(operations.len());
    }
    Ok(CopyPlan {
        source_root,
        destination_root,
        operations,
    })
}

/// Validate the entire plan before writing anything, then commit each file separately.
/// Cancellation keeps already-committed files and reports every remaining item.
pub fn execute_copy(
    plan: &CopyPlan,
    cancel: &AtomicBool,
    mut progress: impl FnMut(usize),
) -> Result<CopyReport, FileError> {
    let mut report = CopyReport {
        source_root: plan.source_root.clone(),
        destination_root: plan.destination_root.clone(),
        items: Vec::with_capacity(plan.operations.len()),
        cancelled: false,
    };
    if cancel.load(Ordering::Relaxed) {
        report.cancelled = true;
    } else {
        for operation in &plan.operations {
            match verify_operation(plan, operation, cancel) {
                Ok(()) => {}
                Err(FileError::Cancelled) => {
                    report.cancelled = true;
                    break;
                }
                Err(error) => return Err(error),
            }
        }
    }
    for operation in &plan.operations {
        let status = if report.cancelled {
            CopyItemStatus::NotRun
        } else {
            match copy_one(plan, operation, cancel) {
                Ok(()) => CopyItemStatus::Copied,
                Err(FileError::Cancelled) => {
                    report.cancelled = true;
                    CopyItemStatus::Cancelled
                }
                Err(error) => CopyItemStatus::Failed(error.to_string()),
            }
        };
        report.items.push(CopyItemResult {
            relative_path: operation.relative_path.clone(),
            status,
        });
        progress(report.items.len());
        if cancel.load(Ordering::Relaxed) {
            report.cancelled = true;
        }
    }
    Ok(report)
}

fn canonical_root(root: &Path) -> Result<PathBuf, FileError> {
    let canonical = fs::canonicalize(root).map_err(|e| io_error(root, e))?;
    let metadata = fs::symlink_metadata(&canonical).map_err(|e| io_error(&canonical, e))?;
    if !metadata.is_dir() {
        return Err(FileError::NotDirectory(canonical));
    }
    if is_link(&metadata) {
        return Err(FileError::UnsafeCopyPath(canonical));
    }
    Ok(canonical)
}

fn is_link(metadata: &Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return true;
        }
    }
    false
}

fn validate_relative(relative: &Path) -> Result<(), FileError> {
    if relative.as_os_str().is_empty()
        || relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(FileError::UnsafeCopyPath(relative.to_owned()));
    }
    #[cfg(windows)]
    if relative.to_string_lossy().contains(':') {
        return Err(FileError::UnsafeCopyPath(relative.to_owned()));
    }
    Ok(())
}

fn checked_path(root: &Path, relative: &Path, allow_missing: bool) -> Result<PathBuf, FileError> {
    validate_relative(relative)?;
    let root_metadata = fs::symlink_metadata(root).map_err(|e| io_error(root, e))?;
    if is_link(&root_metadata) || !root_metadata.is_dir() {
        return Err(FileError::UnsafeCopyPath(root.to_owned()));
    }
    let components: Vec<_> = relative.components().collect();
    let mut path = root.to_owned();
    for (index, component) in components.iter().enumerate() {
        path.push(component.as_os_str());
        match fs::symlink_metadata(&path) {
            Ok(metadata) => {
                if is_link(&metadata) {
                    return Err(FileError::UnsafeCopyPath(path));
                }
                if index + 1 < components.len() && !metadata.is_dir() {
                    return Err(FileError::NotDirectory(path));
                }
                if index + 1 == components.len() && !metadata.is_file() {
                    return Err(FileError::NotRegularFile(path));
                }
                let canonical = fs::canonicalize(&path).map_err(|e| io_error(&path, e))?;
                if !canonical.starts_with(root) {
                    return Err(FileError::UnsafeCopyPath(path));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && allow_missing => {}
            Err(error) => return Err(io_error(&path, error)),
        }
    }
    Ok(path)
}

fn fingerprint(path: &Path, cancel: &AtomicBool) -> Result<Fingerprint, FileError> {
    let mut file = File::open(path).map_err(|e| io_error(path, e))?;
    let before = file.metadata().map_err(|e| io_error(path, e))?;
    if !before.is_file() {
        return Err(FileError::NotRegularFile(path.to_owned()));
    }
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0u8; 65536];
    loop {
        check_cancel(cancel)?;
        let n = file.read(&mut buffer).map_err(|e| io_error(path, e))?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }
    let after = file.metadata().map_err(|e| io_error(path, e))?;
    if before.len() != hasher.count()
        || before.len() != after.len()
        || before.modified().ok() != after.modified().ok()
    {
        return Err(FileError::ChangedOnDisk(path.to_owned()));
    }
    Ok(Fingerprint {
        length: after.len(),
        modified: after.modified().ok(),
        digest: hasher.finalize(),
    })
}

fn optional_fingerprint(
    path: &Path,
    cancel: &AtomicBool,
) -> Result<Option<Fingerprint>, FileError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if is_link(&metadata) {
                return Err(FileError::UnsafeCopyPath(path.to_owned()));
            }
            fingerprint(path, cancel).map(Some)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(io_error(path, error)),
    }
}

fn ensure_writable(path: &Path) -> Result<(), FileError> {
    let metadata = fs::metadata(path).map_err(|e| io_error(path, e))?;
    if metadata.permissions().readonly() {
        return Err(io_error(
            path,
            std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "Destination is read-only",
            ),
        ));
    }
    Ok(())
}

fn verify_operation(
    plan: &CopyPlan,
    operation: &CopyOperation,
    cancel: &AtomicBool,
) -> Result<(), FileError> {
    let source = checked_path(&plan.source_root, &operation.relative_path, false)?;
    let destination = checked_path(&plan.destination_root, &operation.relative_path, true)?;
    if fingerprint(&source, cancel)? != operation.source {
        return Err(FileError::ChangedOnDisk(source));
    }
    if optional_fingerprint(&destination, cancel)? != operation.destination {
        return Err(FileError::ChangedOnDisk(destination));
    }
    if operation.overwrites {
        ensure_writable(&destination)?;
    }
    Ok(())
}

fn create_parents(root: &Path, relative: &Path) -> Result<(), FileError> {
    let mut path = root.to_owned();
    if let Some(parent) = relative.parent() {
        for component in parent.components() {
            path.push(component.as_os_str());
            match fs::create_dir(&path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(io_error(&path, error)),
            }
            let metadata = fs::symlink_metadata(&path).map_err(|e| io_error(&path, e))?;
            if is_link(&metadata) || !metadata.is_dir() {
                return Err(FileError::UnsafeCopyPath(path));
            }
        }
    }
    Ok(())
}

fn copy_one(
    plan: &CopyPlan,
    operation: &CopyOperation,
    cancel: &AtomicBool,
) -> Result<(), FileError> {
    check_cancel(cancel)?;
    verify_operation(plan, operation, cancel)?;
    create_parents(&plan.destination_root, &operation.relative_path)?;
    let source = checked_path(&plan.source_root, &operation.relative_path, false)?;
    let destination = checked_path(&plan.destination_root, &operation.relative_path, true)?;
    let parent = destination
        .parent()
        .ok_or_else(|| FileError::UnsafeCopyPath(destination.clone()))?;
    let mut temporary =
        tempfile::NamedTempFile::new_in(parent).map_err(|e| io_error(&destination, e))?;
    let mut input = File::open(&source).map_err(|e| io_error(&source, e))?;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0u8; 65536];
    loop {
        check_cancel(cancel)?;
        let n = input.read(&mut buffer).map_err(|e| io_error(&source, e))?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
        temporary
            .write_all(&buffer[..n])
            .map_err(|e| io_error(&destination, e))?;
    }
    if hasher.finalize() != operation.source.digest || hasher.count() != operation.source.length {
        return Err(FileError::ChangedOnDisk(source));
    }
    temporary
        .as_file()
        .sync_all()
        .map_err(|e| io_error(&destination, e))?;
    check_cancel(cancel)?;
    // Recheck paths and both byte fingerprints immediately before committing.
    verify_operation(plan, operation, cancel)?;
    if operation.overwrites {
        let permissions = fs::metadata(&destination)
            .map_err(|e| io_error(&destination, e))?
            .permissions();
        temporary
            .as_file()
            .set_permissions(permissions)
            .map_err(|e| io_error(&destination, e))?;
        temporary
            .persist(&destination)
            .map_err(|e| io_error(&destination, e.error))?;
    } else {
        temporary
            .persist_noclobber(&destination)
            .map_err(|e| io_error(&destination, e.error))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copy_creates_nested_files_and_overwrites_only_previewed_targets() {
        let source = tempfile::tempdir().unwrap();
        let destination = tempfile::tempdir().unwrap();
        fs::create_dir(source.path().join("nested")).unwrap();
        let bytes = vec![0x9b; 150_000];
        fs::write(source.path().join("nested/中文.bin"), &bytes).unwrap();
        fs::write(source.path().join("existing"), "new").unwrap();
        fs::write(destination.path().join("existing"), "old").unwrap();
        let cancel = AtomicBool::new(false);
        let plan = prepare_copy(
            source.path(),
            destination.path(),
            &["nested/中文.bin".into(), "existing".into()],
            &cancel,
            |_| {},
        )
        .unwrap();
        assert_eq!(plan.overwrite_count(), 1);
        assert!(!destination.path().join("nested").exists());
        let report = execute_copy(&plan, &cancel, |_| {}).unwrap();
        assert_eq!(report.copied_count(), 2);
        assert_eq!(report.failed_count(), 0);
        assert_eq!(
            fs::read(destination.path().join("nested/中文.bin")).unwrap(),
            bytes
        );
        assert_eq!(
            fs::read_to_string(destination.path().join("existing")).unwrap(),
            "new"
        );
    }

    #[test]
    fn same_size_same_time_changes_invalidate_the_whole_plan_before_writing() {
        let source = tempfile::tempdir().unwrap();
        let destination = tempfile::tempdir().unwrap();
        for name in ["a", "b"] {
            fs::write(source.path().join(name), "old").unwrap();
        }
        let cancel = AtomicBool::new(false);
        let plan = prepare_copy(
            source.path(),
            destination.path(),
            &["a".into(), "b".into()],
            &cancel,
            |_| {},
        )
        .unwrap();
        let path = source.path().join("b");
        let modified = fs::metadata(&path).unwrap().modified().unwrap();
        fs::write(&path, "new").unwrap();
        File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(modified))
            .unwrap();
        assert!(matches!(
            execute_copy(&plan, &cancel, |_| {}),
            Err(FileError::ChangedOnDisk(_))
        ));
        assert!(!destination.path().join("a").exists());
    }

    #[test]
    fn late_destination_creation_is_reported_as_partial_failure_without_overwrite() {
        let source = tempfile::tempdir().unwrap();
        let destination = tempfile::tempdir().unwrap();
        for name in ["a", "b"] {
            fs::write(source.path().join(name), "source").unwrap();
        }
        let cancel = AtomicBool::new(false);
        let plan = prepare_copy(
            source.path(),
            destination.path(),
            &["a".into(), "b".into()],
            &cancel,
            |_| {},
        )
        .unwrap();
        let report = execute_copy(&plan, &cancel, |count| {
            if count == 1 {
                fs::write(destination.path().join("b"), "external").unwrap();
            }
        })
        .unwrap();
        assert_eq!(report.copied_count(), 1);
        assert_eq!(report.failed_count(), 1);
        assert_eq!(
            fs::read_to_string(destination.path().join("b")).unwrap(),
            "external"
        );
    }

    #[test]
    fn cancelling_a_batch_preserves_committed_results_and_marks_remaining_files() {
        let source = tempfile::tempdir().unwrap();
        let destination = tempfile::tempdir().unwrap();
        for name in ["a", "b"] {
            fs::write(source.path().join(name), "source").unwrap();
        }
        let cancel = AtomicBool::new(false);
        let plan = prepare_copy(
            source.path(),
            destination.path(),
            &["a".into(), "b".into()],
            &cancel,
            |_| {},
        )
        .unwrap();
        let report = execute_copy(&plan, &cancel, |count| {
            if count == 1 {
                cancel.store(true, Ordering::Relaxed);
            }
        })
        .unwrap();
        assert!(report.cancelled);
        assert_eq!(report.items[0].status, CopyItemStatus::Copied);
        assert_eq!(report.items[1].status, CopyItemStatus::NotRun);
        assert!(!destination.path().join("b").exists());
    }

    #[test]
    fn traversal_absolute_paths_and_type_conflicts_are_rejected() {
        let source = tempfile::tempdir().unwrap();
        let destination = tempfile::tempdir().unwrap();
        fs::write(source.path().join("a"), "a").unwrap();
        let cancel = AtomicBool::new(false);
        for path in [PathBuf::from("../a"), source.path().join("a")] {
            assert!(matches!(
                prepare_copy(source.path(), destination.path(), &[path], &cancel, |_| {}),
                Err(FileError::UnsafeCopyPath(_))
            ));
        }
        fs::create_dir(destination.path().join("a")).unwrap();
        assert!(matches!(
            prepare_copy(
                source.path(),
                destination.path(),
                &["a".into()],
                &cancel,
                |_| {}
            ),
            Err(FileError::NotRegularFile(_))
        ));
    }

    #[cfg(windows)]
    #[test]
    fn destination_symlinks_cannot_escape_the_selected_root() {
        use std::os::windows::fs::symlink_dir;
        let source = tempfile::tempdir().unwrap();
        let destination = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::create_dir(source.path().join("linked")).unwrap();
        fs::write(source.path().join("linked/a"), "a").unwrap();
        // Creating symlinks can require developer mode; the junction metadata check
        // is always active, while this extra test runs when symlinks are permitted.
        if symlink_dir(outside.path(), destination.path().join("linked")).is_err() {
            return;
        }
        let result = prepare_copy(
            source.path(),
            destination.path(),
            &["linked/a".into()],
            &AtomicBool::new(false),
            |_| {},
        );
        assert!(matches!(result, Err(FileError::UnsafeCopyPath(_))));
        assert!(!outside.path().join("a").exists());
    }
}
