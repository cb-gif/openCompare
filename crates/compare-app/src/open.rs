use crate::TextSession;
use compare_core::CompareOptions;
use compare_fs::{
    files_equal, load_document, load_preview, read_preview_page, verify_preview, FileError,
    LoadedDocument, PreviewCursor, TextEncoding,
};
use std::{
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
};

pub fn open_text_session(
    left: &Path,
    right: &Path,
    left_encoding: Option<TextEncoding>,
    right_encoding: Option<TextEncoding>,
    options: CompareOptions,
    cancel: &AtomicBool,
) -> Result<TextSession, FileError> {
    if cancel.load(Ordering::Relaxed) {
        return Err(FileError::Cancelled);
    }
    let normal = (|| {
        let left = load_document(left, left_encoding)?;
        if cancel.load(Ordering::Relaxed) {
            return Err(FileError::Cancelled);
        }
        let right = load_document(right, right_encoding)?;
        if cancel.load(Ordering::Relaxed) {
            return Err(FileError::Cancelled);
        }
        Ok((left, right))
    })();
    match normal {
        Ok((left, right)) => Ok(TextSession::new(left, right, options)),
        Err(FileError::TooLarge(_) | FileError::TooManyLines(_)) => {
            let left_document = load_preview(left, left_encoding, cancel)?;
            let right_document = load_preview(right, right_encoding, cancel)?;
            let equal = files_equal(left, right, cancel)?;
            verify_preview(&left_document)?;
            verify_preview(&right_document)?;
            let mut session =
                TextSession::new(left_document, right_document, CompareOptions::default());
            session.byte_equal = Some(equal);
            Ok(session)
        }
        Err(error) => Err(error),
    }
}

pub fn load_preview_session_page(
    left: &LoadedDocument,
    right: &LoadedDocument,
    cursors: [PreviewCursor; 2],
    byte_equal: bool,
    cancel: &AtomicBool,
) -> Result<TextSession, FileError> {
    let left = read_preview_page(left, cursors[0], cancel)?;
    let right = read_preview_page(right, cursors[1], cancel)?;
    let mut session = TextSession::new(left, right, CompareOptions::default());
    session.byte_equal = Some(byte_equal);
    Ok(session)
}

#[cfg(test)]
mod tests {
    use super::*;
    use compare_core::{MergeError, Side};
    use compare_fs::{MAX_EDITABLE_LINES, MAX_TEXT_BYTES};
    use std::{
        fs,
        io::{Seek, SeekFrom, Write},
    };

    #[test]
    fn large_file_differences_outside_the_first_page_are_not_reported_equal() {
        let directory = tempfile::tempdir().unwrap();
        let left = directory.path().join("left.txt");
        let right = directory.path().join("right.txt");
        let text = "same\n".repeat(MAX_EDITABLE_LINES + 1);
        fs::write(&left, &text).unwrap();
        fs::write(&right, &text).unwrap();
        let mut file = fs::OpenOptions::new().write(true).open(&right).unwrap();
        file.seek(SeekFrom::End(-5)).unwrap();
        file.write_all(b"diff\n").unwrap();
        drop(file);
        let mut session = open_text_session(
            &left,
            &right,
            None,
            None,
            CompareOptions::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert!(session.is_read_only());
        assert_eq!(session.byte_equal, Some(false));
        assert!(session.diff.as_ref().unwrap().hunks.is_empty());
        assert_eq!(session.accept(0, Side::Left), Err(MergeError::ReadOnly));
        let before = session.right.document.shared_text();
        session.edit(Side::Right, "attempted edit");
        assert_eq!(session.right.document.shared_text(), before);
        assert!(!session.is_dirty());
    }

    #[test]
    fn files_over_the_old_byte_limit_open_as_bounded_preview_pages() {
        let directory = tempfile::tempdir().unwrap();
        let left = directory.path().join("left.txt");
        let right = directory.path().join("right.txt");
        let text = "x".repeat(MAX_TEXT_BYTES as usize + 1);
        fs::write(&left, &text).unwrap();
        fs::write(&right, &text).unwrap();
        let cancel = AtomicBool::new(false);
        let session = open_text_session(
            &left,
            &right,
            None,
            None,
            CompareOptions::default(),
            &cancel,
        )
        .unwrap();
        assert_eq!(session.byte_equal, Some(true));
        assert!(session.left.document.text().len() <= compare_fs::PREVIEW_MAX_BYTES);
        let cursor = [
            session.left.preview.as_ref().unwrap().end,
            session.right.preview.as_ref().unwrap().end,
        ];
        let next = load_preview_session_page(&session.left, &session.right, cursor, true, &cancel)
            .unwrap();
        assert_eq!(next.left.preview.as_ref().unwrap().cursor, cursor[0]);
        assert_eq!(next.byte_equal, Some(true));
    }
}
