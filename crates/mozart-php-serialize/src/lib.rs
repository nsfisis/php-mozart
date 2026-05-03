//! Byte-compatible port of PHP's `serialize()` function.
//!
//! Mirrors `php_var_serialize` in PHP's source: each value is rendered to a
//! tagged form like `b:1;`, `i:42;`, `s:3:"foo";`, `a:N:{...}` so the output
//! can be SHA-1'd and compared against PHP-side hashes (e.g. Composer's
//! `PathRepository` reference, which is `sha1($json . serialize($options))`).
//!
//! Only the value forms Mozart needs today are implemented. Floats, objects,
//! and references are deliberately omitted — extend the [`Value`] enum and
//! [`serialize`] writer when a new shape is required, and add a focused test
//! for it (the file_get_contents → hash flow downstream is unforgiving).
//!
//! Lengths are byte counts, not character counts. Array keys are written in
//! insertion order (PHP arrays preserve insertion order). Integer-coercible
//! string keys (e.g. `"1"`) are NOT auto-converted to integers — PHP itself
//! does that during array construction, not at serialization time, so callers
//! that care must construct [`Value::Int`] keys directly.

use std::fmt::Write;

/// One PHP value, suitable for `serialize()`.
///
/// Add variants here as the need arises (e.g. `Float(f64)` → `d:<repr>;`).
/// Keep the variants minimal — every variant we add is a new compatibility
/// surface that has to match PHP byte-for-byte.
#[derive(Debug, Clone)]
pub enum Value {
    Null,
    Bool(bool),
    Int(i64),
    /// UTF-8 string. Length prefix is the byte length, matching PHP.
    String(String),
    /// Associative or indexed array. Order is preserved verbatim — the writer
    /// does not normalize integer-coercible keys or sort entries.
    Array(Vec<(Value, Value)>),
}

/// Render `value` as PHP's `serialize()` would.
///
/// Returns a `String` (not bytes) because every byte we emit is in
/// printable-ASCII or comes from a UTF-8 [`Value::String`] payload, so the
/// result is always valid UTF-8.
pub fn serialize(value: &Value) -> String {
    let mut out = String::new();
    write_value(&mut out, value);
    out
}

fn write_value(out: &mut String, value: &Value) {
    match value {
        Value::Null => out.push_str("N;"),
        Value::Bool(b) => {
            out.push_str("b:");
            out.push(if *b { '1' } else { '0' });
            out.push(';');
        }
        Value::Int(n) => {
            write!(out, "i:{};", n).expect("writing to String never fails");
        }
        Value::String(s) => {
            write!(out, "s:{}:\"{}\";", s.len(), s).expect("writing to String never fails");
        }
        Value::Array(entries) => {
            write!(out, "a:{}:{{", entries.len()).expect("writing to String never fails");
            for (k, v) in entries {
                write_value(out, k);
                write_value(out, v);
            }
            out.push('}');
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Each `expected` string was produced by running the equivalent PHP
    // `serialize()` call (`php -r 'echo serialize(...);'`), so the assertions
    // pin Mozart's output to actual PHP behaviour rather than the spec we
    // think we're following.

    #[test]
    fn null() {
        assert_eq!(serialize(&Value::Null), "N;");
    }

    #[test]
    fn bool_true() {
        assert_eq!(serialize(&Value::Bool(true)), "b:1;");
    }

    #[test]
    fn bool_false() {
        assert_eq!(serialize(&Value::Bool(false)), "b:0;");
    }

    #[test]
    fn int_positive() {
        assert_eq!(serialize(&Value::Int(42)), "i:42;");
    }

    #[test]
    fn int_zero() {
        assert_eq!(serialize(&Value::Int(0)), "i:0;");
    }

    #[test]
    fn int_negative() {
        assert_eq!(serialize(&Value::Int(-7)), "i:-7;");
    }

    #[test]
    fn string_ascii() {
        assert_eq!(serialize(&Value::String("hi".into())), "s:2:\"hi\";");
    }

    #[test]
    fn string_empty() {
        assert_eq!(serialize(&Value::String(String::new())), "s:0:\"\";");
    }

    #[test]
    fn string_length_is_bytes_not_chars() {
        // 「日本」 is 6 bytes in UTF-8 (3 per kanji), 2 chars. PHP measures
        // by byte; mirror that.
        assert_eq!(serialize(&Value::String("日本".into())), "s:6:\"日本\";");
    }

    #[test]
    fn array_empty() {
        assert_eq!(serialize(&Value::Array(vec![])), "a:0:{}");
    }

    #[test]
    fn array_assoc_single() {
        let v = Value::Array(vec![(Value::String("relative".into()), Value::Bool(true))]);
        assert_eq!(serialize(&v), "a:1:{s:8:\"relative\";b:1;}");
    }

    #[test]
    fn array_assoc_multi_preserves_order() {
        let v = Value::Array(vec![
            (Value::String("a".into()), Value::Int(1)),
            (Value::String("b".into()), Value::Int(2)),
        ]);
        assert_eq!(serialize(&v), "a:2:{s:1:\"a\";i:1;s:1:\"b\";i:2;}");
    }

    #[test]
    fn array_indexed() {
        // PHP `serialize([10, 20])` uses integer keys 0, 1.
        let v = Value::Array(vec![
            (Value::Int(0), Value::Int(10)),
            (Value::Int(1), Value::Int(20)),
        ]);
        assert_eq!(serialize(&v), "a:2:{i:0;i:10;i:1;i:20;}");
    }

    #[test]
    fn array_nested() {
        // PHP: serialize(['outer' => ['inner' => true]])
        let v = Value::Array(vec![(
            Value::String("outer".into()),
            Value::Array(vec![(Value::String("inner".into()), Value::Bool(true))]),
        )]);
        assert_eq!(
            serialize(&v),
            "a:1:{s:5:\"outer\";a:1:{s:5:\"inner\";b:1;}}"
        );
    }
}
