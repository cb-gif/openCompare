use crate::{DocumentSnapshot, Side};
use std::ops::Range;

pub const MAX_SEARCH_MATCHES: usize = 10_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchMatch {
    pub side: Side,
    pub line: usize,
    /// UTF-8 byte offsets in the original line, before case conversion.
    pub bytes: Range<usize>,
}

#[derive(Debug, Clone)]
pub struct SearchResults {
    pub left_revision: u64,
    pub right_revision: u64,
    pub matches: Vec<SearchMatch>,
    pub truncated: bool,
    pub cancelled: bool,
}

/// Literal, single-line search. Case-insensitive matching uses Unicode lowercase,
/// with expanding characters mapped back to the original UTF-8 byte offsets.
pub fn search_documents(
    left: &DocumentSnapshot,
    right: &DocumentSnapshot,
    query: &str,
    case_sensitive: bool,
    cancelled: impl Fn() -> bool,
) -> SearchResults {
    let mut result = SearchResults {
        left_revision: left.revision,
        right_revision: right.revision,
        matches: Vec::new(),
        truncated: false,
        cancelled: false,
    };
    if query.is_empty() || query.contains(['\r', '\n']) {
        return result;
    }
    let needle = if case_sensitive {
        query.to_owned()
    } else {
        query.to_lowercase()
    };
    for (side, document) in [(Side::Left, left), (Side::Right, right)] {
        for line in 0..document.line_count() {
            if cancelled() {
                result.cancelled = true;
                return result;
            }
            let text = document.line_content(line);
            let remaining = MAX_SEARCH_MATCHES + 1 - result.matches.len();
            let ranges = find_in_line(text, &needle, case_sensitive, remaining);
            result
                .matches
                .extend(
                    ranges
                        .into_iter()
                        .map(|bytes| SearchMatch { side, line, bytes }),
                );
            if result.matches.len() > MAX_SEARCH_MATCHES {
                result.matches.truncate(MAX_SEARCH_MATCHES);
                result.truncated = true;
                return result;
            }
        }
    }
    result
}

fn find_in_line(text: &str, needle: &str, case_sensitive: bool, limit: usize) -> Vec<Range<usize>> {
    let folded;
    let haystack = if case_sensitive {
        text
    } else {
        folded = text.to_lowercase();
        &folded
    };
    let ranges: Vec<_> = haystack
        .match_indices(needle)
        .take(limit)
        .map(|(start, value)| start..start + value.len())
        .collect();
    if case_sensitive || text.is_ascii() || ranges.is_empty() {
        return ranges;
    }
    // Keep memory proportional to the result limit, not to a potentially huge line.
    let mut mapped = vec![0..0; ranges.len()];
    let (mut start_index, mut end_index, mut folded_offset) = (0, 0, 0);
    for (original_offset, character) in text.char_indices() {
        folded_offset += character.to_lowercase().map(char::len_utf8).sum::<usize>();
        while start_index < ranges.len() && ranges[start_index].start < folded_offset {
            mapped[start_index].start = original_offset;
            start_index += 1;
        }
        while end_index < ranges.len() && ranges[end_index].end <= folded_offset {
            mapped[end_index].end = original_offset + character.len_utf8();
            end_index += 1;
        }
        if end_index == ranges.len() {
            break;
        }
    }
    mapped
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unicode_case_expansion_maps_back_to_original_bytes() {
        let left = DocumentSnapshot::new("İstanbul 中文 İstanbul\r\n", 4);
        let right = DocumentSnapshot::new("istanbul 中文\n", 8);
        let result = search_documents(&left, &right, "i", false, || false);
        assert_eq!(result.matches.len(), 3);
        for hit in &result.matches[..2] {
            assert_eq!(&left.line_content(hit.line)[hit.bytes.clone()], "İ");
        }
        assert_eq!((result.left_revision, result.right_revision), (4, 8));
        let chinese = search_documents(&left, &right, "中文", true, || false);
        assert_eq!(chinese.matches.len(), 2);
        assert_eq!(
            &left.line_content(0)[chinese.matches[0].bytes.clone()],
            "中文"
        );
    }

    #[test]
    fn search_handles_empty_case_sensitive_and_missing_final_newline() {
        let left = DocumentSnapshot::new("Hello\nhello", 0);
        let right = DocumentSnapshot::new("", 0);
        assert_eq!(
            search_documents(&left, &right, "hello", true, || false).matches[0].line,
            1
        );
        assert_eq!(
            search_documents(&left, &right, "hello", false, || false)
                .matches
                .len(),
            2
        );
        assert!(search_documents(&left, &right, "", false, || false)
            .matches
            .is_empty());
    }

    #[test]
    fn result_limits_and_cancellation_are_explicit() {
        let left = DocumentSnapshot::new("a".repeat(MAX_SEARCH_MATCHES + 20), 0);
        let right = DocumentSnapshot::new("", 0);
        let result = search_documents(&left, &right, "a", true, || false);
        assert_eq!(result.matches.len(), MAX_SEARCH_MATCHES);
        assert!(result.truncated);
        assert!(search_documents(&left, &right, "a", true, || true).cancelled);
    }
}
