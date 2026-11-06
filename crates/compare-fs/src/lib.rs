//! Explicit text decoding, guarded saves, and exact directory comparison.
mod copy;
use compare_core::DocumentSnapshot;
pub use copy::{
    execute_copy, prepare_copy, CopyItemResult, CopyItemStatus, CopyOperation, CopyPlan, CopyReport,
};
use encoding_rs::GBK;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};
use thiserror::Error;
use walkdir::WalkDir;

pub const MAX_TEXT_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Debug, Error)]
pub enum FileError {
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{0}: text files larger than 16 MiB are not editable in this prototype")]
    TooLarge(PathBuf),
    #[error("Invalid {0} text. Select the correct encoding; invalid bytes are never replaced.")]
    InvalidEncoding(&'static str),
    #[error(
        "Binary data cannot be opened as text. Use directory comparison to compare its bytes."
    )]
    Binary,
    #[error("The text contains characters that cannot be represented as {0}.")]
    CannotEncode(&'static str),
    #[error("{0}: the file changed on disk. Reopen it before saving.")]
    ChangedOnDisk(PathBuf),
    #[error("{0}: saving through a symbolic link is disabled; open its target directly")]
    SymlinkSave(PathBuf),
    #[error("{0}: expected a directory")]
    NotDirectory(PathBuf),
    #[error("Comparison cancelled")]
    Cancelled,
    #[error("{0}: copying through a link, reparse point, or unsafe relative path is disabled")]
    UnsafeCopyPath(PathBuf),
    #[error("{0}: only regular files can be copied")]
    NotRegularFile(PathBuf),
    #[error("The source and destination refer to the same path: {0}")]
    SameCopyPath(PathBuf),
    #[error("Select at least one regular file to copy")]
    EmptyCopyPlan,
}

fn io_error(path: &Path, source: std::io::Error) -> FileError {
    FileError::Io {
        path: path.to_owned(),
        source,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TextEncoding {
    Utf8,
    Utf16Le,
    Utf16Be,
    Gbk,
}

impl TextEncoding {
    pub fn label(self) -> &'static str {
        match self {
            Self::Utf8 => "UTF-8",
            Self::Utf16Le => "UTF-16 LE",
            Self::Utf16Be => "UTF-16 BE",
            Self::Gbk => "GBK",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct TextFormat {
    pub encoding: TextEncoding,
    pub bom: bool,
}

pub fn decode(
    bytes: &[u8],
    selected: Option<TextEncoding>,
) -> Result<(String, TextFormat), FileError> {
    let detected = if bytes.starts_with(&[0xef, 0xbb, 0xbf]) {
        Some((TextEncoding::Utf8, 3))
    } else if bytes.starts_with(&[0xff, 0xfe]) {
        Some((TextEncoding::Utf16Le, 2))
    } else if bytes.starts_with(&[0xfe, 0xff]) {
        Some((TextEncoding::Utf16Be, 2))
    } else {
        None
    };
    let encoding = selected
        .or(detected.map(|(enc, _)| enc))
        .unwrap_or(TextEncoding::Utf8);
    if detected.is_some_and(|(enc, _)| enc != encoding) {
        return Err(FileError::InvalidEncoding(encoding.label()));
    }
    let bom = detected.is_some();
    let content = &bytes[detected.map_or(0, |(_, n)| n)..];
    let text = match encoding {
        TextEncoding::Utf8 => std::str::from_utf8(content)
            .map(str::to_owned)
            .map_err(|_| FileError::InvalidEncoding("UTF-8 (try GBK or UTF-16)"))?,
        TextEncoding::Utf16Le | TextEncoding::Utf16Be => {
            let (chunks, remainder) = content.as_chunks::<2>();
            if !remainder.is_empty() {
                return Err(FileError::InvalidEncoding(encoding.label()));
            }
            let units: Vec<_> = chunks
                .iter()
                .map(|b| {
                    if encoding == TextEncoding::Utf16Le {
                        u16::from_le_bytes([b[0], b[1]])
                    } else {
                        u16::from_be_bytes([b[0], b[1]])
                    }
                })
                .collect();
            String::from_utf16(&units).map_err(|_| FileError::InvalidEncoding(encoding.label()))?
        }
        TextEncoding::Gbk => GBK
            .decode_without_bom_handling_and_without_replacement(content)
            .ok_or(FileError::InvalidEncoding("GBK"))?
            .into_owned(),
    };
    if text.contains('\0') {
        return Err(FileError::Binary);
    }
    Ok((text, TextFormat { encoding, bom }))
}

pub fn encode(text: &str, format: TextFormat) -> Result<Vec<u8>, FileError> {
    let mut bytes = Vec::new();
    if format.bom {
        bytes.extend_from_slice(match format.encoding {
            TextEncoding::Utf8 => &[0xef, 0xbb, 0xbf],
            TextEncoding::Utf16Le => &[0xff, 0xfe],
            TextEncoding::Utf16Be => &[0xfe, 0xff],
            TextEncoding::Gbk => &[],
        });
    }
    match format.encoding {
        TextEncoding::Utf8 => bytes.extend_from_slice(text.as_bytes()),
        TextEncoding::Utf16Le | TextEncoding::Utf16Be => {
            for unit in text.encode_utf16() {
                bytes.extend_from_slice(&if format.encoding == TextEncoding::Utf16Le {
                    unit.to_le_bytes()
                } else {
                    unit.to_be_bytes()
                });
            }
        }
        TextEncoding::Gbk => {
            let (encoded, _, had_errors) = GBK.encode(text);
            if had_errors {
                return Err(FileError::CannotEncode("GBK"));
            }
            bytes.extend_from_slice(&encoded);
        }
    }
    Ok(bytes)
}

#[derive(Debug, Clone)]
pub struct LoadedDocument {
    pub path: Option<PathBuf>,
    pub document: DocumentSnapshot,
    pub format: TextFormat,
    original_bytes: Arc<[u8]>,
    original_text: Arc<str>,
    line_ending: &'static str,
}

impl LoadedDocument {
    pub fn from_text(text: &str) -> Self {
        Self {
            path: None,
            document: DocumentSnapshot::new(text, 0),
            format: TextFormat {
                encoding: TextEncoding::Utf8,
                bom: false,
            },
            original_bytes: text.as_bytes().into(),
            original_text: text.into(),
            line_ending: line_ending_label(text),
        }
    }

    pub fn is_dirty(&self) -> bool {
        self.document.text() != self.original_text.as_ref()
    }

    pub fn replace_text(&mut self, text: impl Into<Arc<str>>) {
        self.document = DocumentSnapshot::new(text, self.document.revision + 1);
        self.line_ending = line_ending_label(self.document.text());
    }

    pub fn line_ending_label(&self) -> &'static str {
        self.line_ending
    }
}

fn line_ending_label(text: &str) -> &'static str {
    let crlf = text.matches("\r\n").count();
    let lf = text.bytes().filter(|b| *b == b'\n').count();
    if crlf > 0 && crlf != lf {
        "Mixed EOL"
    } else if crlf > 0 {
        "CRLF"
    } else if lf > 0 {
        "LF"
    } else {
        "No EOL"
    }
}

pub fn load_document(
    path: &Path,
    encoding: Option<TextEncoding>,
) -> Result<LoadedDocument, FileError> {
    let metadata = fs::metadata(path).map_err(|e| io_error(path, e))?;
    if metadata.len() > MAX_TEXT_BYTES {
        return Err(FileError::TooLarge(path.to_owned()));
    }
    // Limit the read even if the file grows after metadata was queried.
    let file = File::open(path).map_err(|e| io_error(path, e))?;
    let mut bytes = Vec::new();
    file.take(MAX_TEXT_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| io_error(path, e))?;
    if bytes.len() as u64 > MAX_TEXT_BYTES {
        return Err(FileError::TooLarge(path.to_owned()));
    }
    let (text, format) = decode(&bytes, encoding)?;
    // Keep an absolute path without resolving the final symlink; saving it must be rejected.
    let absolute = std::path::absolute(path).map_err(|e| io_error(path, e))?;
    let document = DocumentSnapshot::new(text, 0);
    let line_ending = line_ending_label(document.text());
    Ok(LoadedDocument {
        path: Some(absolute),
        original_text: document.shared_text(),
        document,
        format,
        original_bytes: bytes.into(),
        line_ending,
    })
}

/// Prepare all bytes before touching the original, then replace from a same-directory temp file.
pub fn save_document(document: &mut LoadedDocument) -> Result<(), FileError> {
    let path = document.path.as_ref().ok_or_else(|| {
        io_error(
            Path::new("untitled"),
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "Open a file before saving",
            ),
        )
    })?;
    if !document.is_dirty() {
        return Ok(());
    }
    let metadata = fs::symlink_metadata(path).map_err(|e| io_error(path, e))?;
    if metadata.file_type().is_symlink() {
        return Err(FileError::SymlinkSave(path.clone()));
    }
    if metadata.permissions().readonly() {
        return Err(io_error(
            path,
            std::io::Error::new(std::io::ErrorKind::PermissionDenied, "File is read-only"),
        ));
    }
    verify_original(path, &document.original_bytes)?;
    let bytes = encode(document.document.text(), document.format)?;
    let parent = path.parent().unwrap_or(Path::new("."));
    let mut temporary = tempfile::NamedTempFile::new_in(parent).map_err(|e| io_error(path, e))?;
    temporary.write_all(&bytes).map_err(|e| io_error(path, e))?;
    temporary
        .as_file()
        .set_permissions(metadata.permissions())
        .map_err(|e| io_error(path, e))?;
    temporary
        .as_file()
        .sync_all()
        .map_err(|e| io_error(path, e))?;
    // Catch changes made while encoding or flushing. Portable filesystems do not offer CAS writes.
    verify_original(path, &document.original_bytes)?;
    temporary
        .persist(path)
        .map_err(|e| io_error(path, e.error))?;
    document.original_bytes = bytes.into();
    document.original_text = document.document.shared_text();
    Ok(())
}

fn verify_original(path: &Path, expected: &[u8]) -> Result<(), FileError> {
    let file = File::open(path).map_err(|e| io_error(path, e))?;
    let mut current = Vec::new();
    file.take(expected.len() as u64 + 1)
        .read_to_end(&mut current)
        .map_err(|e| io_error(path, e))?;
    if current != expected {
        return Err(FileError::ChangedOnDisk(path.to_owned()));
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum EntryKind {
    File,
    Directory,
    Symlink,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum EntryStatus {
    Equal,
    Different,
    LeftOnly,
    RightOnly,
    TypeConflict,
    Directory,
    Error,
}

#[derive(Debug, Clone, Serialize)]
pub struct DirectoryEntryPair {
    pub relative_path: PathBuf,
    pub left_kind: Option<EntryKind>,
    pub right_kind: Option<EntryKind>,
    pub left_size: Option<u64>,
    pub right_size: Option<u64>,
    pub status: EntryStatus,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DirectoryReport {
    pub left: PathBuf,
    pub right: PathBuf,
    pub entries: Vec<DirectoryEntryPair>,
}

impl DirectoryReport {
    pub fn different_count(&self) -> usize {
        self.entries
            .iter()
            .filter(|e| !matches!(e.status, EntryStatus::Equal | EntryStatus::Directory))
            .count()
    }
    pub fn error_count(&self) -> usize {
        self.entries
            .iter()
            .filter(|e| e.status == EntryStatus::Error)
            .count()
    }
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

pub fn compare_directories(
    left: &Path,
    right: &Path,
    cancel: &AtomicBool,
    mut progress: impl FnMut(usize),
) -> Result<DirectoryReport, FileError> {
    let left = std::path::absolute(left).map_err(|e| io_error(left, e))?;
    let right = std::path::absolute(right).map_err(|e| io_error(right, e))?;
    for path in [&left, &right] {
        if !fs::metadata(path).map_err(|e| io_error(path, e))?.is_dir() {
            return Err(FileError::NotDirectory(path.to_owned()));
        }
    }
    let mut pairs: BTreeMap<PathBuf, Pair> = BTreeMap::new();
    let mut count = 0usize;
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
                Err(error) => {
                    let path = error.path().unwrap_or(root).to_owned();
                    (
                        path,
                        Entry {
                            kind: EntryKind::Other,
                            size: 0,
                            error: Some(error.to_string()),
                        },
                    )
                }
            };
            let relative = path.strip_prefix(root).unwrap_or(&path).to_owned();
            let pair = pairs.entry(relative).or_default();
            if is_left {
                pair.left = Some(item);
            } else {
                pair.right = Some(item);
            }
            count += 1;
            if count.is_multiple_of(64) {
                progress(count);
            }
        }
    }
    let mut entries = Vec::with_capacity(pairs.len());
    for (relative_path, pair) in pairs {
        check_cancel(cancel)?;
        let mut error = pair
            .left
            .as_ref()
            .and_then(|e| e.error.clone())
            .or_else(|| pair.right.as_ref().and_then(|e| e.error.clone()));
        let mut status = match (&pair.left, &pair.right) {
            (Some(l), Some(r)) if l.kind != r.kind => EntryStatus::TypeConflict,
            (Some(l), Some(_)) if l.kind == EntryKind::Directory => EntryStatus::Directory,
            (Some(l), Some(r)) if l.kind == EntryKind::File => {
                if l.size != r.size {
                    EntryStatus::Different
                } else {
                    match files_equal(
                        &left.join(&relative_path),
                        &right.join(&relative_path),
                        cancel,
                    ) {
                        Ok(true) => EntryStatus::Equal,
                        Ok(false) => EntryStatus::Different,
                        Err(FileError::Cancelled) => return Err(FileError::Cancelled),
                        Err(e) => {
                            error = Some(e.to_string());
                            EntryStatus::Error
                        }
                    }
                }
            }
            (Some(l), Some(_)) if l.kind == EntryKind::Symlink => {
                match (
                    fs::read_link(left.join(&relative_path)),
                    fs::read_link(right.join(&relative_path)),
                ) {
                    (Ok(l), Ok(r)) => {
                        if l == r {
                            EntryStatus::Equal
                        } else {
                            EntryStatus::Different
                        }
                    }
                    (Err(e), _) | (_, Err(e)) => {
                        error = Some(e.to_string());
                        EntryStatus::Error
                    }
                }
            }
            (Some(_), Some(_)) => {
                error = Some("Unsupported file type".into());
                EntryStatus::Error
            }
            (Some(_), None) => EntryStatus::LeftOnly,
            (None, Some(_)) => EntryStatus::RightOnly,
            (None, None) => unreachable!(),
        };
        if error.is_some() {
            status = EntryStatus::Error;
        }
        entries.push(DirectoryEntryPair {
            relative_path,
            status,
            error,
            left_kind: pair.left.as_ref().map(|e| e.kind),
            right_kind: pair.right.as_ref().map(|e| e.kind),
            left_size: pair.left.as_ref().map(|e| e.size),
            right_size: pair.right.as_ref().map(|e| e.size),
        });
        count += 1;
        if count.is_multiple_of(64) {
            progress(count);
        }
    }
    progress(count);
    Ok(DirectoryReport {
        left: left.to_owned(),
        right: right.to_owned(),
        entries,
    })
}

fn check_cancel(cancel: &AtomicBool) -> Result<(), FileError> {
    if cancel.load(Ordering::Relaxed) {
        Err(FileError::Cancelled)
    } else {
        Ok(())
    }
}

pub fn files_equal(left: &Path, right: &Path, cancel: &AtomicBool) -> Result<bool, FileError> {
    let mut l = File::open(left).map_err(|e| io_error(left, e))?;
    let mut r = File::open(right).map_err(|e| io_error(right, e))?;
    let lm = l.metadata().map_err(|e| io_error(left, e))?;
    let rm = r.metadata().map_err(|e| io_error(right, e))?;
    if lm.len() != rm.len() {
        return Ok(false);
    }
    let (mut lb, mut rb) = ([0u8; 65536], [0u8; 65536]);
    loop {
        check_cancel(cancel)?;
        let ln = read_chunk(&mut l, &mut lb).map_err(|e| io_error(left, e))?;
        let rn = read_chunk(&mut r, &mut rb).map_err(|e| io_error(right, e))?;
        if ln != rn || lb[..ln] != rb[..rn] {
            return Ok(false);
        }
        if ln == 0 {
            break;
        }
    }
    for (file, before, path) in [(&l, lm, left), (&r, rm, right)] {
        let after = file.metadata().map_err(|e| io_error(path, e))?;
        if before.len() != after.len() || before.modified().ok() != after.modified().ok() {
            return Err(FileError::ChangedOnDisk(path.to_owned()));
        }
    }
    Ok(true)
}

fn read_chunk(reader: &mut impl Read, buffer: &mut [u8]) -> std::io::Result<usize> {
    let mut total = 0;
    while total < buffer.len() {
        let n = reader.read(&mut buffer[total..])?;
        if n == 0 {
            break;
        }
        total += n;
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directory_reports_keep_absolute_roots_for_session_restore() {
        let current = std::env::current_dir().unwrap();
        let directory = tempfile::tempdir_in(&current).unwrap();
        let relative = directory.path().strip_prefix(&current).unwrap();
        fs::write(directory.path().join("sample"), "content").unwrap();
        let report =
            compare_directories(relative, relative, &AtomicBool::new(false), |_| {}).unwrap();
        assert!(report.left.is_absolute());
        assert!(report.right.is_absolute());
        assert_eq!(report.entries[0].status, EntryStatus::Equal);
    }

    #[test]
    fn all_encodings_round_trip_bom_unicode_and_mixed_newlines() {
        for encoding in [
            TextEncoding::Utf8,
            TextEncoding::Utf16Le,
            TextEncoding::Utf16Be,
            TextEncoding::Gbk,
        ] {
            for bom in [false, true] {
                if encoding == TextEncoding::Gbk && bom {
                    continue;
                }
                let format = TextFormat { encoding, bom };
                let original = "中文\r\nhello\n末尾";
                let bytes = encode(original, format).unwrap();
                let (text, detected) = decode(&bytes, Some(encoding)).unwrap();
                assert_eq!(text, original);
                assert_eq!(encode(&text, detected).unwrap(), bytes);
            }
        }
    }

    #[test]
    fn malformed_and_unrepresentable_data_is_rejected() {
        assert!(decode(&[0xff], None).is_err());
        assert!(decode(&[0xff, 0xfe, 0x00], None).is_err());
        assert!(decode(&[0xff, 0xfe, 0x00, 0xd8], None).is_err());
        assert!(encode(
            "🙂",
            TextFormat {
                encoding: TextEncoding::Gbk,
                bom: false
            }
        )
        .is_err());
        assert!(matches!(decode(b"a\0b", None), Err(FileError::Binary)));
    }

    #[test]
    fn save_preserves_format_and_refuses_external_changes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sample.txt");
        let format = TextFormat {
            encoding: TextEncoding::Utf16Le,
            bom: true,
        };
        let original = encode("中文\r\nend", format).unwrap();
        fs::write(&path, &original).unwrap();
        let mut doc = load_document(&path, None).unwrap();
        save_document(&mut doc).unwrap();
        assert_eq!(fs::read(&path).unwrap(), original);
        doc.replace_text("修改\r\nend");
        save_document(&mut doc).unwrap();
        assert!(!doc.is_dirty());
        assert_eq!(
            fs::read(&path).unwrap(),
            encode("修改\r\nend", format).unwrap()
        );
        doc.replace_text("next");
        fs::write(&path, b"external").unwrap();
        assert!(matches!(
            save_document(&mut doc),
            Err(FileError::ChangedOnDisk(_))
        ));
        assert_eq!(fs::read(&path).unwrap(), b"external");
    }

    #[test]
    fn directory_comparison_checks_same_size_content_and_type_conflicts() {
        let left = tempfile::tempdir().unwrap();
        let right = tempfile::tempdir().unwrap();
        for (name, l, r) in [
            ("same", b"abc".as_slice(), b"abc".as_slice()),
            ("different", b"aaa", b"bbb"),
        ] {
            fs::write(left.path().join(name), l).unwrap();
            fs::write(right.path().join(name), r).unwrap();
        }
        fs::write(left.path().join("left-only"), "x").unwrap();
        fs::write(right.path().join("right-only"), "x").unwrap();
        fs::create_dir(left.path().join("conflict")).unwrap();
        fs::write(right.path().join("conflict"), "x").unwrap();
        let report =
            compare_directories(left.path(), right.path(), &AtomicBool::new(false), |_| {})
                .unwrap();
        let status = |name: &str| {
            report
                .entries
                .iter()
                .find(|e| e.relative_path == Path::new(name))
                .unwrap()
                .status
        };
        assert_eq!(status("same"), EntryStatus::Equal);
        assert_eq!(status("different"), EntryStatus::Different);
        assert_eq!(status("left-only"), EntryStatus::LeftOnly);
        assert_eq!(status("right-only"), EntryStatus::RightOnly);
        assert_eq!(status("conflict"), EntryStatus::TypeConflict);
        assert_eq!(report.different_count(), 4);
    }

    #[test]
    fn cancelled_scan_never_reports_equality() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a"), "a").unwrap();
        assert!(matches!(
            compare_directories(dir.path(), dir.path(), &AtomicBool::new(true), |_| {}),
            Err(FileError::Cancelled)
        ));
    }
}
