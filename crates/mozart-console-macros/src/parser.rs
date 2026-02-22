const KNOWN_TAGS: &[&str] = &[
    "info",
    "comment",
    "error",
    "question",
    "highlight",
    "warning",
];

#[derive(Debug, Clone, PartialEq)]
pub enum Segment {
    Plain(String),
    Tagged { tag: String, content: String },
}

pub fn parse_format_string(input: &str) -> Result<Vec<Segment>, String> {
    let mut segments: Vec<Segment> = Vec::new();
    let mut chars = input.char_indices().peekable();
    let mut plain_buf = String::new();

    while let Some(&(i, ch)) = chars.peek() {
        if ch == '<' {
            // Try to match an opening tag
            if let Some((tag, after_tag)) = try_parse_open_tag(input, i) {
                // Flush plain buffer
                if !plain_buf.is_empty() {
                    segments.push(Segment::Plain(std::mem::take(&mut plain_buf)));
                }

                // Advance past the opening tag
                while chars.peek().is_some_and(|&(j, _)| j < after_tag) {
                    chars.next();
                }

                // Collect content until closing tag
                let closing = format!("</{tag}>");
                let content_start = after_tag;
                let Some(close_pos) = input[content_start..].find(&closing) else {
                    return Err(format!("unclosed <{tag}> tag"));
                };
                let content_end = content_start + close_pos;
                let content = &input[content_start..content_end];

                // Check for nested tags
                if contains_known_tag(content) {
                    return Err(format!("nested tags are not supported inside <{tag}>"));
                }

                segments.push(Segment::Tagged {
                    tag: tag.to_string(),
                    content: content.to_string(),
                });

                // Advance past the closing tag
                let after_close = content_end + closing.len();
                while chars.peek().is_some_and(|&(j, _)| j < after_close) {
                    chars.next();
                }
            } else {
                // Not a known tag, treat as literal
                plain_buf.push(ch);
                chars.next();
            }
        } else {
            plain_buf.push(ch);
            chars.next();
        }
    }

    if !plain_buf.is_empty() {
        segments.push(Segment::Plain(plain_buf));
    }

    Ok(segments)
}

/// Try to parse an opening tag like `<info>` at position `pos`.
/// Returns `(tag_name, byte_index_after_closing_angle)` on success.
fn try_parse_open_tag(input: &str, pos: usize) -> Option<(&str, usize)> {
    let rest = &input[pos + 1..]; // skip '<'
    // Must not start with '/'
    if rest.starts_with('/') {
        return None;
    }
    let end = rest.find('>')?;
    let tag_name = &rest[..end];
    if KNOWN_TAGS.contains(&tag_name) {
        Some((tag_name, pos + 1 + end + 1))
    } else {
        None
    }
}

/// Check if a string contains any known opening tag (for nesting detection).
fn contains_known_tag(s: &str) -> bool {
    for tag in KNOWN_TAGS {
        if s.contains(&format!("<{tag}>")) {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_only() {
        let result = parse_format_string("hello world").unwrap();
        assert_eq!(result, vec![Segment::Plain("hello world".into())]);
    }

    #[test]
    fn single_tag() {
        let result = parse_format_string("<info>hello</info>").unwrap();
        assert_eq!(
            result,
            vec![Segment::Tagged {
                tag: "info".into(),
                content: "hello".into()
            }]
        );
    }

    #[test]
    fn tag_with_placeholder() {
        let result = parse_format_string("<info>Removing {name}</info>").unwrap();
        assert_eq!(
            result,
            vec![Segment::Tagged {
                tag: "info".into(),
                content: "Removing {name}".into()
            }]
        );
    }

    #[test]
    fn multiple_tags() {
        let result = parse_format_string("<info>{}</info> : <comment>{}</comment>").unwrap();
        assert_eq!(
            result,
            vec![
                Segment::Tagged {
                    tag: "info".into(),
                    content: "{}".into()
                },
                Segment::Plain(" : ".into()),
                Segment::Tagged {
                    tag: "comment".into(),
                    content: "{}".into()
                },
            ]
        );
    }

    #[test]
    fn all_tag_types() {
        for tag in KNOWN_TAGS {
            let input = format!("<{tag}>text</{tag}>");
            let result = parse_format_string(&input).unwrap();
            assert_eq!(
                result,
                vec![Segment::Tagged {
                    tag: tag.to_string(),
                    content: "text".into()
                }]
            );
        }
    }

    #[test]
    fn unknown_tag_treated_as_literal() {
        let result = parse_format_string("<bold>text</bold>").unwrap();
        assert_eq!(result, vec![Segment::Plain("<bold>text</bold>".into())]);
    }

    #[test]
    fn unclosed_tag_error() {
        let result = parse_format_string("<info>text");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("unclosed"));
    }

    #[test]
    fn nested_tag_error() {
        let result = parse_format_string("<info><comment>text</comment></info>");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("nested"));
    }

    #[test]
    fn escaped_braces() {
        let result = parse_format_string("<info>{{literal}}</info>").unwrap();
        assert_eq!(
            result,
            vec![Segment::Tagged {
                tag: "info".into(),
                content: "{{literal}}".into()
            }]
        );
    }

    #[test]
    fn adjacent_tags() {
        let result = parse_format_string("<info>a</info><comment>b</comment>").unwrap();
        assert_eq!(
            result,
            vec![
                Segment::Tagged {
                    tag: "info".into(),
                    content: "a".into()
                },
                Segment::Tagged {
                    tag: "comment".into(),
                    content: "b".into()
                },
            ]
        );
    }

    #[test]
    fn plain_before_and_after_tag() {
        let result = parse_format_string("before <info>middle</info> after").unwrap();
        assert_eq!(
            result,
            vec![
                Segment::Plain("before ".into()),
                Segment::Tagged {
                    tag: "info".into(),
                    content: "middle".into()
                },
                Segment::Plain(" after".into()),
            ]
        );
    }

    #[test]
    fn empty_content_tag() {
        let result = parse_format_string("<info></info>").unwrap();
        assert_eq!(
            result,
            vec![Segment::Tagged {
                tag: "info".into(),
                content: String::new()
            }]
        );
    }
}
