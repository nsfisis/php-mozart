use proc_macro2::TokenStream;
use quote::quote;
use syn::Expr;
use syn::punctuated::Punctuated;

use crate::parser::Segment;

/// Returns true if the string contains any format placeholders (`{}`, `{name}`, `{0}`, `{:<10}`, etc.)
/// but not escaped braces `{{` or `}}`.
fn has_placeholders(s: &str) -> bool {
    let mut chars = s.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '{' {
            match chars.peek() {
                Some('{') => {
                    chars.next(); // skip escaped
                }
                _ => return true,
            }
        } else if ch == '}' && chars.peek() == Some(&'}') {
            chars.next(); // skip escaped
        }
    }
    false
}

/// Count implicit positional placeholders (`{}` and `{:spec}`) in a format string.
/// Named (`{name}`) and numbered (`{0}`) placeholders are NOT counted
/// since they don't consume positional arguments.
fn count_positional_placeholders(s: &str) -> usize {
    let mut count = 0;
    let mut chars = s.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '{' {
            match chars.peek() {
                Some('{') => {
                    chars.next(); // escaped
                }
                Some('}') => {
                    // `{}`  — implicit positional
                    count += 1;
                    chars.next();
                }
                Some(':') => {
                    // `{:spec}` — implicit positional with format spec
                    count += 1;
                    for c in chars.by_ref() {
                        if c == '}' {
                            break;
                        }
                    }
                }
                Some(c) if c.is_ascii_digit() => {
                    // `{0}`, `{0:spec}` — explicit positional, skip
                    for c in chars.by_ref() {
                        if c == '}' {
                            break;
                        }
                    }
                }
                _ => {
                    // `{name}` or `{name:spec}` — named, skip
                    for c in chars.by_ref() {
                        if c == '}' {
                            break;
                        }
                    }
                }
            }
        } else if ch == '}' && chars.peek() == Some(&'}') {
            chars.next();
        }
    }
    count
}

pub fn generate(
    segments: &[Segment],
    extra_args: &Punctuated<Expr, syn::Token![,]>,
) -> TokenStream {
    // Single segment: pass all extra args
    if segments.len() == 1 {
        return generate_single(&segments[0], extra_args);
    }

    // Multiple segments: distribute positional args across segments
    let mut pos = 0usize;
    let mut seg_bindings = Vec::new();
    let mut seg_idents = Vec::new();

    for (i, segment) in segments.iter().enumerate() {
        let content = segment_content(segment);
        let n = count_positional_placeholders(content);
        let end = (pos + n).min(extra_args.len());
        let slice: Punctuated<Expr, syn::Token![,]> = extra_args
            .iter()
            .skip(pos)
            .take(end - pos)
            .cloned()
            .collect();
        pos = end;

        let ident = quote::format_ident!("__seg{}", i);
        let expr = generate_single(segment, &slice);
        seg_bindings.push(quote! { let #ident = #expr; });
        seg_idents.push(ident);
    }

    // Build a format string with one `{}` per segment
    let fmt_str = seg_idents.iter().map(|_| "{}").collect::<Vec<_>>().join("");

    quote! {
        {
            #(#seg_bindings)*
            ::std::format!(#fmt_str, #(#seg_idents),*)
        }
    }
}

fn segment_content(segment: &Segment) -> &str {
    match segment {
        Segment::Plain(s) => s,
        Segment::Tagged { content, .. } => content,
    }
}

fn generate_single(segment: &Segment, args: &Punctuated<Expr, syn::Token![,]>) -> TokenStream {
    match segment {
        Segment::Plain(text) => {
            if has_placeholders(text) {
                let lit = proc_macro2::Literal::string(text);
                quote! { ::std::format!(#lit, #args) }
            } else {
                quote! { ::std::string::String::from(#text) }
            }
        }
        Segment::Tagged { tag, content } => {
            let func = quote::format_ident!("{}", tag);
            if has_placeholders(content) {
                let lit = proc_macro2::Literal::string(content);
                quote! {
                    ::std::string::ToString::to_string(
                        &::mozart_core::console::#func(&::std::format!(#lit, #args))
                    )
                }
            } else {
                quote! {
                    ::std::string::ToString::to_string(
                        &::mozart_core::console::#func(#content)
                    )
                }
            }
        }
    }
}
