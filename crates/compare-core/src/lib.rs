//! UI-independent, lossless text snapshots and aligned two-way differences.
mod search;
pub use search::{search_documents, SearchMatch, SearchResults, MAX_SEARCH_MATCHES};
use serde::{Deserialize, Serialize};
use similar::{ChangeTag, DiffTag, TextDiff};
use std::{
    ops::Range,
    sync::Arc,
    time::{Duration, Instant},
};
use thiserror::Error;

#[derive(Debug, Clone)]
pub struct DocumentSnapshot {
    text: Arc<str>,
    lines: Arc<[Range<usize>]>,
    pub revision: u64,
}

impl DocumentSnapshot {
    pub fn new(text: impl Into<Arc<str>>, revision: u64) -> Self {
        let text = text.into();
        let mut offset = 0;
        let lines: Vec<_> = text
            .split_inclusive('\n')
            .map(|line| {
                let start = offset;
                offset += line.len();
                start..offset
            })
            .collect();
        Self {
            text,
            lines: lines.into(),
            revision,
        }
    }

    pub fn text(&self) -> &str {
        &self.text
    }
    pub fn shared_text(&self) -> Arc<str> {
        self.text.clone()
    }
    pub fn line_count(&self) -> usize {
        self.lines.len()
    }
    pub fn line(&self, index: usize) -> &str {
        &self.text[self.lines[index].clone()]
    }
    pub fn line_content(&self, index: usize) -> &str {
        split_ending(self.line(index)).0
    }

    pub fn line_range_bytes(&self, range: Range<usize>) -> Range<usize> {
        self.line_start(range.start)..self.line_start(range.end)
    }

    fn line_start(&self, index: usize) -> usize {
        self.lines
            .get(index)
            .map_or(self.text.len(), |line| line.start)
    }
}

pub fn split_ending(line: &str) -> (&str, &str) {
    if let Some(content) = line.strip_suffix("\r\n") {
        (content, "\r\n")
    } else if let Some(content) = line.strip_suffix('\n') {
        (content, "\n")
    } else {
        (line, "")
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CompareOptions {
    pub ignore_trailing_whitespace: bool,
    pub ignore_case: bool,
    pub ignore_line_endings: bool,
}

impl CompareOptions {
    fn key(&self, line: &str) -> String {
        let (content, ending) = split_ending(line);
        let content = if self.ignore_trailing_whitespace {
            content.trim_end_matches([' ', '\t'])
        } else {
            content
        };
        let content = if self.ignore_case {
            content.to_lowercase()
        } else {
            content.to_owned()
        };
        if self.ignore_line_endings {
            content
        } else {
            content + ending
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum RowKind {
    Equal,
    Changed,
    LeftOnly,
    RightOnly,
}

#[derive(Debug, Clone)]
pub struct AlignedRow {
    pub left: Option<usize>,
    pub right: Option<usize>,
    pub kind: RowKind,
    pub hunk: Option<usize>,
    /// UTF-8 byte ranges within the original line, always on grapheme boundaries.
    pub left_spans: Vec<Range<usize>>,
    pub right_spans: Vec<Range<usize>>,
}

#[derive(Debug, Clone)]
pub struct DiffHunk {
    pub left: Range<usize>,
    pub right: Range<usize>,
    pub first_row: usize,
}

#[derive(Debug, Clone)]
pub struct DiffResult {
    pub left_revision: u64,
    pub right_revision: u64,
    pub rows: Vec<AlignedRow>,
    pub hunks: Vec<DiffHunk>,
    /// A deadline can coarsen alignment; it never drops unmatched content.
    pub coarse_alignment: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum MergeError {
    #[error("The documents changed; compare again before accepting this block.")]
    StaleResult,
    #[error("This difference block no longer exists.")]
    MissingHunk,
}

pub fn compare(
    left: &DocumentSnapshot,
    right: &DocumentSnapshot,
    options: CompareOptions,
) -> DiffResult {
    let left_keys: Vec<_> = (0..left.line_count())
        .map(|i| options.key(left.line(i)))
        .collect();
    let right_keys: Vec<_> = (0..right.line_count())
        .map(|i| options.key(right.line(i)))
        .collect();
    let left_refs: Vec<_> = left_keys.iter().map(String::as_str).collect();
    let right_refs: Vec<_> = right_keys.iter().map(String::as_str).collect();
    let deadline = Instant::now() + Duration::from_secs(2);
    let diff = TextDiff::configure()
        .deadline(deadline)
        .diff_slices(&left_refs, &right_refs);
    let mut result = DiffResult {
        left_revision: left.revision,
        right_revision: right.revision,
        rows: Vec::new(),
        hunks: Vec::new(),
        coarse_alignment: Instant::now() >= deadline,
    };
    let inline_deadline = Instant::now() + Duration::from_millis(120);
    let mut pending: Option<(Range<usize>, Range<usize>)> = None;
    for op in diff.ops() {
        let old = op.old_range();
        let new = op.new_range();
        if op.tag() == DiffTag::Equal {
            flush_hunk(&mut result, &mut pending, left, right, inline_deadline);
            for (l, r) in old.zip(new) {
                result.rows.push(AlignedRow {
                    left: Some(l),
                    right: Some(r),
                    kind: RowKind::Equal,
                    hunk: None,
                    left_spans: Vec::new(),
                    right_spans: Vec::new(),
                });
            }
        } else if let Some((l, r)) = &mut pending {
            l.end = old.end;
            r.end = new.end;
        } else {
            pending = Some((old, new));
        }
    }
    flush_hunk(&mut result, &mut pending, left, right, inline_deadline);
    result
}

fn flush_hunk(
    result: &mut DiffResult,
    pending: &mut Option<(Range<usize>, Range<usize>)>,
    left: &DocumentSnapshot,
    right: &DocumentSnapshot,
    deadline: Instant,
) {
    let Some((l, r)) = pending.take() else { return };
    let hunk = result.hunks.len();
    result.hunks.push(DiffHunk {
        left: l.clone(),
        right: r.clone(),
        first_row: result.rows.len(),
    });
    for offset in 0..l.len().max(r.len()) {
        let li = (offset < l.len()).then_some(l.start + offset);
        let ri = (offset < r.len()).then_some(r.start + offset);
        let (kind, left_spans, right_spans) = match (li, ri) {
            (Some(a), Some(b)) => {
                let (ls, rs) = inline_spans(left.line_content(a), right.line_content(b), deadline);
                (RowKind::Changed, ls, rs)
            }
            (Some(_), None) => (RowKind::LeftOnly, Vec::new(), Vec::new()),
            (None, Some(_)) => (RowKind::RightOnly, Vec::new(), Vec::new()),
            _ => unreachable!(),
        };
        result.rows.push(AlignedRow {
            left: li,
            right: ri,
            kind,
            hunk: Some(hunk),
            left_spans,
            right_spans,
        });
    }
}

type InlineSpans = (Vec<Range<usize>>, Vec<Range<usize>>);
fn inline_spans(left: &str, right: &str, deadline: Instant) -> InlineSpans {
    if left.len().max(right.len()) > 8192 || Instant::now() >= deadline {
        return (
            std::iter::once(0..left.len()).collect(),
            std::iter::once(0..right.len()).collect(),
        );
    }
    let diff = TextDiff::configure()
        .deadline(deadline)
        .diff_graphemes(left, right);
    let (mut l, mut r) = (0, 0);
    let (mut ls, mut rs) = (Vec::new(), Vec::new());
    for change in diff.iter_all_changes() {
        let n = change.value().len();
        match change.tag() {
            ChangeTag::Equal => {
                l += n;
                r += n;
            }
            ChangeTag::Delete => {
                ls.push(l..l + n);
                l += n;
            }
            ChangeTag::Insert => {
                rs.push(r..r + n);
                r += n;
            }
        }
    }
    (ls, rs)
}

impl DiffResult {
    /// Return the target text after accepting a source block. Neither input is mutated.
    pub fn accept_hunk(
        &self,
        index: usize,
        source: Side,
        left: &DocumentSnapshot,
        right: &DocumentSnapshot,
    ) -> Result<String, MergeError> {
        if left.revision != self.left_revision || right.revision != self.right_revision {
            return Err(MergeError::StaleResult);
        }
        let hunk = self.hunks.get(index).ok_or(MergeError::MissingHunk)?;
        let (from, from_lines, to, to_lines) = match source {
            Side::Left => (left, hunk.left.clone(), right, hunk.right.clone()),
            Side::Right => (right, hunk.right.clone(), left, hunk.left.clone()),
        };
        let mut text = to.text().to_owned();
        text.replace_range(
            to.line_range_bytes(to_lines),
            &from.text()[from.line_range_bytes(from_lines)],
        );
        Ok(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepting_all_blocks_reconstructs_exact_target() {
        let samples = [
            "",
            "\n",
            "a",
            "a\n",
            "a\r\n",
            "a\r\nb\n",
            "你好👩‍💻\n世界",
            "x\nx\nx\n",
            "a\nb\nc\n",
        ];
        for old in samples {
            for new in samples {
                let left = DocumentSnapshot::new(old, 1);
                let mut right = DocumentSnapshot::new(new, 1);
                for _ in 0..20 {
                    let diff = compare(&left, &right, CompareOptions::default());
                    if diff.hunks.is_empty() {
                        break;
                    }
                    right = DocumentSnapshot::new(
                        diff.accept_hunk(0, Side::Left, &left, &right).unwrap(),
                        right.revision + 1,
                    );
                }
                assert_eq!(right.text(), old, "old={old:?}, new={new:?}");
            }
        }
    }

    #[test]
    fn rules_preserve_input_and_alignment() {
        let left = DocumentSnapshot::new("Hello  \r\n", 0);
        let right = DocumentSnapshot::new("hello\n", 0);
        let options = CompareOptions {
            ignore_trailing_whitespace: true,
            ignore_case: true,
            ignore_line_endings: true,
        };
        let diff = compare(&left, &right, options);
        assert!(diff.hunks.is_empty());
        assert_eq!(left.text(), "Hello  \r\n");
        assert_eq!(diff.rows[0].left, Some(0));
    }

    #[test]
    fn stale_blocks_cannot_edit_new_revisions() {
        let left = DocumentSnapshot::new("a", 1);
        let right = DocumentSnapshot::new("b", 1);
        let diff = compare(&left, &right, CompareOptions::default());
        let changed = DocumentSnapshot::new("c", 2);
        assert_eq!(
            diff.accept_hunk(0, Side::Left, &left, &changed),
            Err(MergeError::StaleResult)
        );
    }

    #[test]
    fn inline_spans_respect_unicode_boundaries() {
        let left = DocumentSnapshot::new("你好👩‍💻\n", 0);
        let right = DocumentSnapshot::new("您好👨‍💻\n", 0);
        let diff = compare(&left, &right, CompareOptions::default());
        for row in &diff.rows {
            for range in &row.left_spans {
                assert!(left.line_content(0).get(range.clone()).is_some());
            }
            for range in &row.right_spans {
                assert!(right.line_content(0).get(range.clone()).is_some());
            }
        }
        assert_eq!(diff.hunks.len(), 1);
    }
}
