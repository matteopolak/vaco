//! The override-tag tokenizer: split a `Dialogue:` `Text` field into
//! literal-text and `{...}` tag-block items, without interpreting any tag
//! (that is [`crate::plan`]'s job). GitHub #487 (FT-5.2)'s "parsing" half.

#[derive(Debug, Clone, PartialEq)]
pub enum Item {
    Text(String),
    /// One tag: `name` is the letters (`pos`, `1c`, `fscx`, ...), `arg` is
    /// whatever followed it — the parenthesised contents with the
    /// parentheses stripped, or a bare value up to the next `\` or the
    /// block's end.
    Tag { name: String, arg: Option<String> },
}

/// Tokenize one event's raw `Text` field.
#[must_use]
pub fn tokenize(text: &str) -> Vec<Item> {
    let mut items = Vec::new();
    let mut buf = String::new();
    let mut chars = text.char_indices().peekable();
    let bytes = text.as_bytes();
    while let Some((i, c)) = chars.next() {
        if c == '{' {
            if !buf.is_empty() {
                items.push(Item::Text(std::mem::take(&mut buf)));
            }
            let Some(end) = text[i..].find('}') else {
                // Unterminated block: the reference treats the rest of the
                // line as the block. Consuming it here (dropping any tags
                // in it) is safer than emitting it as literal text, which
                // would show raw tag syntax on screen.
                break;
            };
            let inner = &text[i + 1..i + end];
            items.extend(tokenize_tags(inner));
            // Skip past the consumed block.
            while let Some(&(j, _)) = chars.peek() {
                if j > i + end {
                    break;
                }
                chars.next();
            }
            continue;
        }
        if c == '\\' && bytes.get(i + 1).is_some() {
            let next = text[i + 1..].chars().next().unwrap_or('\\');
            match next {
                'N' | 'n' => {
                    buf.push('\n');
                    chars.next();
                    continue;
                }
                'h' => {
                    buf.push('\u{00A0}');
                    chars.next();
                    continue;
                }
                _ => {}
            }
        }
        buf.push(c);
    }
    if !buf.is_empty() {
        items.push(Item::Text(buf));
    }
    items
}

/// Split the inside of one `{...}` block into individual `\tag` items.
fn tokenize_tags(inner: &str) -> Vec<Item> {
    let mut out = Vec::new();
    let mut rest = inner;
    while let Some(start) = rest.find('\\') {
        rest = &rest[start + 1..];
        let name_len = rest.find(|c: char| !c.is_ascii_alphabetic()).unwrap_or(rest.len());
        let name: String = rest[..name_len].to_ascii_lowercase();
        rest = &rest[name_len..];
        if name.is_empty() {
            continue;
        }
        if rest.starts_with('(') {
            // Parenthesised arg — may itself contain nested parens (`\t`'s
            // own arguments are override tags, which never nest parens
            // further in practice, so a simple depth counter suffices).
            let mut depth = 0usize;
            let mut end = None;
            for (idx, ch) in rest.char_indices() {
                match ch {
                    '(' => depth += 1,
                    ')' => {
                        depth -= 1;
                        if depth == 0 {
                            end = Some(idx);
                            break;
                        }
                    }
                    _ => {}
                }
            }
            if let Some(end) = end {
                out.push(Item::Tag { name, arg: Some(rest[1..end].to_owned()) });
                rest = &rest[end + 1..];
                continue;
            }
            // Unterminated paren: take the rest as the argument.
            out.push(Item::Tag { name, arg: Some(rest[1..].to_owned()) });
            rest = "";
            continue;
        }
        let arg_len = rest.find('\\').unwrap_or(rest.len());
        let arg = &rest[..arg_len];
        out.push(Item::Tag {
            name,
            arg: if arg.is_empty() { None } else { Some(arg.to_owned()) },
        });
        rest = &rest[arg_len..];
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::expect_used, clippy::float_cmp, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn plain_text_with_no_tags() {
        assert_eq!(tokenize("hello"), vec![Item::Text("hello".to_owned())]);
    }

    #[test]
    fn forced_newline_becomes_a_real_newline() {
        assert_eq!(tokenize("a\\Nb"), vec![Item::Text("a\nb".to_owned())]);
    }

    #[test]
    fn a_bare_value_tag() {
        assert_eq!(
            tokenize("{\\b1}bold"),
            vec![Item::Tag { name: "b".to_owned(), arg: Some("1".to_owned()) }, Item::Text("bold".to_owned())]
        );
    }

    #[test]
    fn a_parenthesised_tag() {
        assert_eq!(
            tokenize("{\\pos(100,200)}x"),
            vec![Item::Tag { name: "pos".to_owned(), arg: Some("100,200".to_owned()) }, Item::Text("x".to_owned())]
        );
    }

    #[test]
    fn multiple_tags_in_one_block() {
        let items = tokenize("{\\b1\\i1}x");
        assert_eq!(
            items,
            vec![
                Item::Tag { name: "b".to_owned(), arg: Some("1".to_owned()) },
                Item::Tag { name: "i".to_owned(), arg: Some("1".to_owned()) },
                Item::Text("x".to_owned()),
            ]
        );
    }

    #[test]
    fn unterminated_block_does_not_panic_or_leak_raw_syntax() {
        let items = tokenize("before{\\pos(1,2) never closes");
        assert_eq!(items, vec![Item::Text("before".to_owned())]);
    }

    #[test]
    fn nested_parens_inside_a_t_tag_are_kept_whole() {
        let items = tokenize("{\\t(0,500,\\fscx150)}x");
        assert_eq!(
            items,
            vec![
                Item::Tag { name: "t".to_owned(), arg: Some("0,500,\\fscx150".to_owned()) },
                Item::Text("x".to_owned()),
            ]
        );
    }
}
