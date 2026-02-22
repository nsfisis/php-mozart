use mozart_core::console_format;

#[test]
fn plain_text_no_tags() {
    let result = console_format!("hello world");
    assert_eq!(result, "hello world");
}

#[test]
fn plain_text_with_format_args() {
    let x = 42;
    let result = console_format!("value is {}", x);
    assert_eq!(result, "value is 42");
}

#[test]
fn single_info_tag() {
    // The output should contain the text (colored), verify it contains the raw text
    let result = console_format!("<info>done</info>");
    assert!(result.contains("done"), "expected 'done' in: {result}");
}

#[test]
fn single_tag_with_format_arg() {
    let name = "foo";
    let result = console_format!("<info>Removing {name}</info>");
    assert!(
        result.contains("Removing foo"),
        "expected 'Removing foo' in: {result}"
    );
}

#[test]
fn multiple_tags() {
    let label = "pkg";
    let version = "1.0";
    let result = console_format!("<info>{}</info> : <comment>{}</comment>", label, version);
    assert!(result.contains("pkg"), "expected 'pkg' in: {result}");
    assert!(result.contains("1.0"), "expected '1.0' in: {result}");
    assert!(result.contains(" : "), "expected ' : ' in: {result}");
}

#[test]
fn comment_tag() {
    let result = console_format!("<comment>note</comment>");
    assert!(result.contains("note"));
}

#[test]
fn error_tag() {
    let result = console_format!("<error>fail</error>");
    assert!(result.contains("fail"));
}

#[test]
fn question_tag() {
    let result = console_format!("<question>ask</question>");
    assert!(result.contains("ask"));
}

#[test]
fn highlight_tag() {
    let result = console_format!("<highlight>important</highlight>");
    assert!(result.contains("important"));
}

#[test]
fn warning_tag() {
    let result = console_format!("<warning>caution</warning>");
    assert!(result.contains("caution"));
}

#[test]
fn escaped_braces() {
    let result = console_format!("<info>{{literal}}</info>");
    assert!(
        result.contains("{literal}"),
        "expected '{{literal}}' in: {result}"
    );
}

#[test]
fn tag_with_plain_before_after() {
    let result = console_format!("before <info>middle</info> after");
    assert!(result.contains("before "));
    assert!(result.contains("middle"));
    assert!(result.contains(" after"));
}

#[test]
fn unknown_tag_is_literal() {
    let result = console_format!("<bold>text</bold>");
    assert_eq!(result, "<bold>text</bold>");
}
