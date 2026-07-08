/// The byte range and slice of the completable token ending at `cursor` (a byte
/// offset into `line`): the longest run of identifier characters and dots
/// immediately before the cursor. `None` when that run is empty.
pub fn completable_token(line: &str, cursor: usize) -> Option<(usize, &str)> {
    let cursor = cursor.min(line.len());
    let head = match line.get(..cursor) {
        Some(head) => head,
        None => return None,
    };
    let start = head
        .char_indices()
        .rev()
        .take_while(|(_, c)| c.is_ascii_alphanumeric() || *c == '_' || *c == '.')
        .last()
        .map(|(i, _)| i)
        .unwrap_or(cursor);
    if start == cursor {
        None
    } else {
        Some((start, &line[start..cursor]))
    }
}

/// The longest string that every item starts with.
pub fn longest_common_prefix(items: &[String]) -> String {
    let mut iter = items.iter();
    let mut prefix = match iter.next() {
        Some(first) => first.clone(),
        None => return String::new(),
    };
    for item in iter {
        while !item.starts_with(&prefix) {
            prefix.pop();
            if prefix.is_empty() {
                return String::new();
            }
        }
    }
    prefix
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn token_is_trailing_identifier_run() {
        assert_eq!(completable_token("delog.fi", 8), Some((0, "delog.fi")));
        assert_eq!(completable_token("x = delog.fi", 12), Some((4, "delog.fi")));
        assert_eq!(completable_token("f.t", 3), Some((0, "f.t")));
    }

    #[test]
    fn token_stops_at_cursor_not_end() {
        assert_eq!(completable_token("abc def", 3), Some((0, "abc")));
    }

    #[test]
    fn token_none_when_empty() {
        assert_eq!(completable_token("x = ", 4), None);
        assert_eq!(completable_token("", 0), None);
    }

    #[test]
    fn token_none_when_cursor_not_on_char_boundary() {
        // "é" is two bytes; cursor 1 lands mid-character.
        assert_eq!(completable_token("é", 1), None);
    }

    #[test]
    fn lcp_finds_shared_prefix() {
        assert_eq!(
            longest_common_prefix(&s(&["delog.field", "delog.find", "delog.find_all"])),
            "delog.fi"
        );
        assert_eq!(longest_common_prefix(&s(&["abc"])), "abc");
        assert_eq!(longest_common_prefix(&s(&[])), "");
        assert_eq!(longest_common_prefix(&s(&["ab", "cd"])), "");
    }
}
