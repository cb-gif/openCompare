//! Bounded, encoding-aware pages for large read-only text files.
use super::{
    check_cancel, decode_payload, detect_format, io_error, line_ending_label, FileError,
    LoadedDocument, TextEncoding,
};
use compare_core::DocumentSnapshot;
use std::{
    fs::{self, File, Metadata},
    io::{Read, Seek, SeekFrom},
    path::Path,
    sync::atomic::AtomicBool,
    time::SystemTime,
};

pub const PREVIEW_MAX_BYTES: usize = 1024 * 1024;
pub const PREVIEW_MAX_LINES: usize = 2000;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct PreviewCursor {
    pub byte_offset: u64,
    /// Zero-based line containing the next byte, including continued long lines.
    pub line: u64,
    pub continuation: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileVersion {
    length: u64,
    modified: Option<SystemTime>,
}
impl FileVersion {
    fn new(metadata: &Metadata) -> Self {
        Self {
            length: metadata.len(),
            modified: metadata.modified().ok(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct PreviewInfo {
    pub cursor: PreviewCursor,
    pub end: PreviewCursor,
    pub file_bytes: u64,
    version: FileVersion,
}
impl PreviewInfo {
    pub fn has_more(&self) -> bool {
        self.end.byte_offset < self.file_bytes
    }
}

pub fn load_preview(
    path: &Path,
    encoding: Option<TextEncoding>,
    cancel: &AtomicBool,
) -> Result<LoadedDocument, FileError> {
    load_page(path, encoding, PreviewCursor::default(), None, cancel)
}

pub fn read_preview_page(
    document: &LoadedDocument,
    cursor: PreviewCursor,
    cancel: &AtomicBool,
) -> Result<LoadedDocument, FileError> {
    let info = document
        .preview
        .as_ref()
        .ok_or(FileError::InvalidPreviewPosition)?;
    let path = document
        .path
        .as_deref()
        .ok_or(FileError::InvalidPreviewPosition)?;
    load_page(
        path,
        Some(document.format.encoding),
        cursor,
        Some(info.version),
        cancel,
    )
}

pub fn verify_preview(document: &LoadedDocument) -> Result<(), FileError> {
    let info = document
        .preview
        .as_ref()
        .ok_or(FileError::InvalidPreviewPosition)?;
    let path = document
        .path
        .as_deref()
        .ok_or(FileError::InvalidPreviewPosition)?;
    let metadata = fs::metadata(path).map_err(|e| io_error(path, e))?;
    if FileVersion::new(&metadata) != info.version {
        return Err(FileError::ChangedOnDisk(path.to_owned()));
    }
    Ok(())
}

fn load_page(
    path: &Path,
    encoding: Option<TextEncoding>,
    cursor: PreviewCursor,
    expected: Option<FileVersion>,
    cancel: &AtomicBool,
) -> Result<LoadedDocument, FileError> {
    check_cancel(cancel)?;
    let mut file = File::open(path).map_err(|e| io_error(path, e))?;
    let metadata = file.metadata().map_err(|e| io_error(path, e))?;
    if !metadata.is_file() {
        return Err(FileError::NotRegularFile(path.to_owned()));
    }
    let version = FileVersion::new(&metadata);
    if expected.is_some_and(|old| old != version) {
        return Err(FileError::ChangedOnDisk(path.to_owned()));
    }
    let mut header = [0u8; 4];
    let n = super::read_chunk(&mut file, &mut header).map_err(|e| io_error(path, e))?;
    let (format, bom_bytes) = detect_format(&header[..n], encoding)?;
    let offset = if cursor.byte_offset == 0 {
        bom_bytes as u64
    } else {
        cursor.byte_offset
    };
    if offset > version.length
        || (matches!(
            format.encoding,
            TextEncoding::Utf16Le | TextEncoding::Utf16Be
        ) && !offset.is_multiple_of(2))
    {
        return Err(FileError::InvalidPreviewPosition);
    }
    file.seek(SeekFrom::Start(offset))
        .map_err(|e| io_error(path, e))?;
    let mut bytes = Vec::with_capacity(PREVIEW_MAX_BYTES);
    let mut buffer = [0u8; 65536];
    while bytes.len() < PREVIEW_MAX_BYTES {
        check_cancel(cancel)?;
        let wanted = buffer.len().min(PREVIEW_MAX_BYTES - bytes.len());
        let n = file
            .read(&mut buffer[..wanted])
            .map_err(|e| io_error(path, e))?;
        if n == 0 {
            break;
        }
        bytes.extend_from_slice(&buffer[..n]);
    }
    let eof = offset + bytes.len() as u64 >= version.length;
    let end = page_boundary(&bytes, format.encoding, eof);
    let mut decoded = None;
    let unit = if matches!(
        format.encoding,
        TextEncoding::Utf16Le | TextEncoding::Utf16Be
    ) {
        2
    } else {
        1
    };
    let trim_limit = if eof || end < bytes.len() { 0 } else { 4 };
    let mut last_error = None;
    for trim in (0..=trim_limit).step_by(unit) {
        let Some(length) = end.checked_sub(trim) else {
            break;
        };
        match decode_payload(&bytes[..length], format.encoding) {
            Ok(text) => {
                decoded = Some((text, length));
                break;
            }
            Err(FileError::InvalidEncoding(label)) => {
                last_error = Some(FileError::InvalidEncoding(label));
            }
            Err(error) => return Err(error),
        }
    }
    let (text, consumed) =
        decoded.ok_or_else(|| last_error.unwrap_or(FileError::InvalidPreviewPosition))?;
    if !eof && consumed == 0 {
        return Err(FileError::InvalidPreviewPosition);
    }
    if FileVersion::new(&file.metadata().map_err(|e| io_error(path, e))?) != version {
        return Err(FileError::ChangedOnDisk(path.to_owned()));
    }
    let end_cursor = PreviewCursor {
        byte_offset: offset + consumed as u64,
        line: cursor.line + text.bytes().filter(|b| *b == b'\n').count() as u64,
        continuation: !text.is_empty() && !text.ends_with('\n'),
    };
    let line_ending = line_ending_label(&text);
    let document = DocumentSnapshot::new(text, 0);
    Ok(LoadedDocument {
        path: Some(std::path::absolute(path).map_err(|e| io_error(path, e))?),
        original_text: document.shared_text(),
        document,
        format,
        original_bytes: [].into(),
        line_ending,
        preview: Some(PreviewInfo {
            cursor,
            end: end_cursor,
            file_bytes: version.length,
            version,
        }),
    })
}

fn page_boundary(bytes: &[u8], encoding: TextEncoding, eof: bool) -> usize {
    let mut lines = 0;
    let mut last_newline = None;
    let unit = if matches!(encoding, TextEncoding::Utf16Le | TextEncoding::Utf16Be) {
        2
    } else {
        1
    };
    for offset in (0..bytes.len()).step_by(unit) {
        let newline = match encoding {
            TextEncoding::Utf16Le => bytes.get(offset..offset + 2) == Some(&[10, 0]),
            TextEncoding::Utf16Be => bytes.get(offset..offset + 2) == Some(&[0, 10]),
            _ => bytes[offset] == 10,
        };
        if newline {
            lines += 1;
            last_newline = Some(offset + unit);
            if lines == PREVIEW_MAX_LINES {
                return offset + unit;
            }
        }
    }
    if eof {
        bytes.len()
    } else {
        last_newline.unwrap_or(bytes.len() / unit * unit)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{encode, save_document, TextFormat};

    #[test]
    fn pages_preserve_encodings_newlines_and_final_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("page.txt");
        let text = "中文 text\r\n".repeat(PREVIEW_MAX_LINES) + "最后一行";
        for encoding in [
            TextEncoding::Utf8,
            TextEncoding::Utf16Le,
            TextEncoding::Utf16Be,
            TextEncoding::Gbk,
        ] {
            let bytes = encode(
                &text,
                TextFormat {
                    encoding,
                    bom: encoding != TextEncoding::Gbk,
                },
            )
            .unwrap();
            fs::write(&path, &bytes).unwrap();
            let cancel = AtomicBool::new(false);
            let first = load_preview(&path, Some(encoding), &cancel).unwrap();
            assert_eq!(first.document.line_count(), PREVIEW_MAX_LINES);
            let info = first.preview.as_ref().unwrap();
            assert!(info.has_more());
            let second = read_preview_page(&first, info.end, &cancel).unwrap();
            assert_eq!(
                first.document.text().to_owned() + second.document.text(),
                text
            );
            assert_eq!(
                second.preview.as_ref().unwrap().cursor.line,
                PREVIEW_MAX_LINES as u64
            );
            assert!(!second.preview.as_ref().unwrap().has_more());
        }
    }

    #[test]
    fn long_lines_cross_byte_boundaries_without_replacing_characters() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("long.txt");
        let text = "你".repeat(PREVIEW_MAX_BYTES / 3 + 10) + "👩‍💻";
        fs::write(&path, &text).unwrap();
        let cancel = AtomicBool::new(false);
        let first = load_preview(&path, None, &cancel).unwrap();
        let end = first.preview.as_ref().unwrap().end;
        assert!(end.continuation);
        assert!(end.byte_offset <= PREVIEW_MAX_BYTES as u64);
        let second = read_preview_page(&first, end, &cancel).unwrap();
        assert_eq!(
            first.document.text().to_owned() + second.document.text(),
            text
        );
    }

    #[test]
    fn multibyte_and_surrogate_boundaries_round_trip_without_accepting_malformed_eof() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("boundary.txt");
        let cancel = AtomicBool::new(false);
        for encoding in [
            TextEncoding::Utf8,
            TextEncoding::Utf16Le,
            TextEncoding::Utf16Be,
            TextEncoding::Gbk,
        ] {
            let prefix = match encoding {
                TextEncoding::Utf16Le | TextEncoding::Utf16Be => {
                    "a".repeat(PREVIEW_MAX_BYTES / 2 - 1)
                }
                _ => "a".repeat(PREVIEW_MAX_BYTES - 1),
            };
            let text = prefix
                + if encoding == TextEncoding::Gbk {
                    "中文"
                } else {
                    "👩中文"
                };
            let format = TextFormat {
                encoding,
                bom: false,
            };
            let bytes = encode(&text, format).unwrap();
            fs::write(&path, &bytes).unwrap();
            let first = load_preview(&path, Some(encoding), &cancel).unwrap();
            let second =
                read_preview_page(&first, first.preview.as_ref().unwrap().end, &cancel).unwrap();
            assert_eq!(
                first.document.text().to_owned() + second.document.text(),
                text
            );
            let malformed = match encoding {
                TextEncoding::Utf8 => vec![0xf0, 0x9f],
                TextEncoding::Utf16Le => vec![0, 0xd8],
                TextEncoding::Utf16Be => vec![0xd8, 0],
                TextEncoding::Gbk => vec![0x81],
            };
            fs::write(&path, malformed).unwrap();
            assert!(matches!(
                load_preview(&path, Some(encoding), &cancel),
                Err(FileError::InvalidEncoding(_))
            ));
        }
    }

    #[test]
    fn previews_refuse_saves_and_detect_file_changes_between_pages() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file.txt");
        fs::write(&path, "line\n".repeat(PREVIEW_MAX_LINES + 1)).unwrap();
        let cancel = AtomicBool::new(false);
        let mut first = load_preview(&path, None, &cancel).unwrap();
        let end = first.preview.as_ref().unwrap().end;
        first.replace_text("changed");
        assert!(matches!(
            save_document(&mut first),
            Err(FileError::ReadOnlyPreview)
        ));
        fs::write(&path, "external change").unwrap();
        assert!(matches!(
            read_preview_page(&first, end, &cancel),
            Err(FileError::ChangedOnDisk(_))
        ));
    }

    #[test]
    fn later_pages_keep_literal_bom_characters_and_cancellation_is_explicit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file.txt");
        fs::write(&path, "line\n".repeat(PREVIEW_MAX_LINES) + "\u{feff}tail").unwrap();
        let first = load_preview(&path, None, &AtomicBool::new(false)).unwrap();
        let next = first.preview.as_ref().unwrap().end;
        let second = read_preview_page(&first, next, &AtomicBool::new(false)).unwrap();
        assert_eq!(second.document.text(), "\u{feff}tail");
        assert!(matches!(
            read_preview_page(&first, next, &AtomicBool::new(true)),
            Err(FileError::Cancelled)
        ));
    }
}
