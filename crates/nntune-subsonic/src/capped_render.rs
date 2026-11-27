//! Two-pass, bounded document rendering for callers that account for the input graph.
//!
//! The caller must charge the returned buffer and the retained `Value` graph before
//! rendering. This module does not bound their construction or runtime copies.

use std::fmt::{self, Write};

use crate::{envelope::is_valid_jsonp_callback, value::Value};

const MAX_DEPTH: usize = 64;

#[derive(Clone, Copy)]
pub enum Format<'a> {
    Json,
    Jsonp(&'a str),
    Xml,
}

#[derive(Debug, PartialEq, Eq)]
pub enum RenderError {
    TooLarge,
    InvalidCallback,
}

// The first pass counts without allocation; the second writes to the exact-sized
// buffer. Both use the same traversal and immutable input.
struct Sink<'a> {
    bytes: Option<&'a mut [u8]>,
    len: usize,
    limit: usize,
}

impl Write for Sink<'_> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let end = self.len.checked_add(s.len()).ok_or(fmt::Error)?;
        if end > self.limit {
            return Err(fmt::Error);
        }
        if let Some(bytes) = &mut self.bytes {
            bytes
                .get_mut(self.len..end)
                .ok_or(fmt::Error)?
                .copy_from_slice(s.as_bytes());
        }
        self.len = end;
        Ok(())
    }
}

/// Count a complete document before the caller charges and allocates its buffer.
/// Equality with `limit` is allowed, with at most 64 nesting levels below the
/// root. Invalid callbacks are never reflected.
pub fn encoded_len(doc: &Value, format: Format<'_>, limit: usize) -> Result<usize, RenderError> {
    if let Format::Jsonp(callback) = format {
        if !is_valid_jsonp_callback(callback) {
            return Err(RenderError::InvalidCallback);
        }
    }
    let mut count = Sink {
        bytes: None,
        len: 0,
        limit,
    };
    document(doc, format, &mut count).map_err(|_| RenderError::TooLarge)?;
    Ok(count.len)
}

/// Render into a caller-owned buffer after it has charged the allocation.
/// Discard the buffer on error; it may contain a partial document.
pub fn render_into(
    doc: &Value,
    format: Format<'_>,
    bytes: &mut [u8],
) -> Result<usize, RenderError> {
    let len = encoded_len(doc, format, bytes.len())?;
    let mut out = Sink {
        bytes: Some(bytes),
        len: 0,
        limit: len,
    };
    document(doc, format, &mut out).map_err(|_| RenderError::TooLarge)?;
    Ok(out.len)
}

fn document(doc: &Value, format: Format<'_>, out: &mut Sink<'_>) -> fmt::Result {
    match format {
        Format::Json | Format::Jsonp(_) => {
            if let Format::Jsonp(callback) = format {
                write!(out, "/**/{callback}(")?;
            }
            out.write_str("{\"subsonic-response\":")?;
            json(doc, out, 0)?;
            out.write_str("}")?;
            if matches!(format, Format::Jsonp(_)) {
                out.write_str(");")?;
            }
            Ok(())
        }
        Format::Xml => {
            out.write_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>")?;
            let mut has_root = false;
            xml_root(doc, out, 0, &mut has_root)?;
            if has_root {
                Ok(())
            } else {
                Err(fmt::Error)
            }
        }
    }
}

fn json(value: &Value, out: &mut Sink<'_>, depth: usize) -> fmt::Result {
    if depth > MAX_DEPTH {
        return Err(fmt::Error);
    }
    match value {
        Value::Null => out.write_str("null"),
        Value::Bool(b) => out.write_str(if *b { "true" } else { "false" }),
        Value::Int(n) => write!(out, "{n}"),
        Value::Str(s) => json_string(s, out),
        Value::List(items) => {
            out.write_char('[')?;
            let mut first = true;
            for item in items.iter().filter(|v| !matches!(v, Value::Null)) {
                if !first {
                    out.write_char(',')?;
                }
                first = false;
                json(item, out, depth + 1)?;
            }
            out.write_char(']')
        }
        Value::Map(entries) => {
            out.write_char('{')?;
            let mut first = true;
            for (key, value) in entries.iter().filter(|(_, v)| !matches!(v, Value::Null)) {
                if !first {
                    out.write_char(',')?;
                }
                first = false;
                json_string(key, out)?;
                out.write_char(':')?;
                json(value, out, depth + 1)?;
            }
            out.write_char('}')
        }
    }
}

fn json_string(s: &str, out: &mut Sink<'_>) -> fmt::Result {
    out.write_char('"')?;
    for c in s.chars() {
        match c {
            '"' => out.write_str("\\\"")?,
            '\\' => out.write_str("\\\\")?,
            '\n' => out.write_str("\\n")?,
            '\r' => out.write_str("\\r")?,
            '\t' => out.write_str("\\t")?,
            c if (c as u32) < 0x20 => write!(out, "\\u{:04x}", c as u32)?,
            c => out.write_char(c)?,
        }
    }
    out.write_char('"')
}

fn scalar(value: &Value, out: &mut Sink<'_>) -> fmt::Result {
    match value {
        Value::Bool(b) => out.write_str(if *b { "true" } else { "false" }),
        Value::Int(n) => write!(out, "{n}"),
        Value::Str(s) => {
            for c in s.chars() {
                match c {
                    '&' => out.write_str("&amp;")?,
                    '<' => out.write_str("&lt;")?,
                    '>' => out.write_str("&gt;")?,
                    '"' => out.write_str("&quot;")?,
                    c => out.write_char(c)?,
                }
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

fn xml_root(value: &Value, out: &mut Sink<'_>, depth: usize, has_root: &mut bool) -> fmt::Result {
    if depth > MAX_DEPTH {
        return Err(fmt::Error);
    }
    if let Value::List(items) = value {
        for item in items {
            xml_root(item, out, depth + 1, has_root)?;
        }
        return Ok(());
    }
    if *has_root {
        return Err(fmt::Error);
    }
    *has_root = true;
    xml(value, "subsonic-response", out, depth)
}

fn xml(value: &Value, name: &str, out: &mut Sink<'_>, depth: usize) -> fmt::Result {
    if depth > MAX_DEPTH {
        return Err(fmt::Error);
    }
    match value {
        Value::Map(entries) => {
            write!(out, "<{name}")?;
            let mut text = None;
            let mut has_children = false;
            for (key, val) in entries {
                match val {
                    Value::Null => {}
                    Value::Bool(_) | Value::Int(_) | Value::Str(_) => {
                        if depth == MAX_DEPTH {
                            return Err(fmt::Error);
                        }
                        if key == "value" && matches!(val, Value::Str(_)) {
                            if let Value::Str(s) = val {
                                text = Some(s.as_str());
                            }
                        } else {
                            write!(out, " {key}=\"")?;
                            scalar(val, out)?;
                            out.write_char('"')?;
                        }
                    }
                    Value::List(items) => {
                        has_children |= items.iter().any(|v| !matches!(v, Value::Null))
                    }
                    Value::Map(_) => has_children = true,
                }
            }
            if !has_children && text.is_none() {
                return out.write_str("/>");
            }
            out.write_char('>')?;
            if let Some(text) = text {
                // Reuse the scalar escaping without cloning the string.
                for c in text.chars() {
                    match c {
                        '&' => out.write_str("&amp;")?,
                        '<' => out.write_str("&lt;")?,
                        '>' => out.write_str("&gt;")?,
                        '"' => out.write_str("&quot;")?,
                        c => out.write_char(c)?,
                    }
                }
            }
            for (key, val) in entries {
                match val {
                    Value::Map(_) => xml(val, key, out, depth + 1)?,
                    Value::List(items) => {
                        for item in items.iter().filter(|v| !matches!(v, Value::Null)) {
                            xml(item, key, out, depth + 2)?;
                        }
                    }
                    _ => {}
                }
            }
            write!(out, "</{name}>")
        }
        Value::List(_) => Err(fmt::Error),
        _ => {
            write!(out, "<{name}>")?;
            scalar(value, out)?;
            write!(out, "</{name}>")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envelope::{render_json, render_jsonp, render_xml};

    #[test]
    fn matches_existing_renderers_and_enforces_exact_limit() {
        let doc = Value::map(vec![
            ("name", Value::str_value("é<&\"\\\n\u{1}")),
            (
                "item",
                Value::List(vec![
                    Value::Null,
                    Value::int_value(-42),
                    Value::map(vec![("value", Value::str_value("<&"))]),
                ]),
            ),
        ]);
        for (format, expected) in [
            (Format::Json, render_json(&doc)),
            (
                Format::Jsonp("cb.test"),
                render_jsonp(&doc, Some("cb.test")).0,
            ),
            (Format::Xml, render_xml(&doc)),
        ] {
            assert_eq!(
                encoded_len(&doc, format, expected.len()),
                Ok(expected.len())
            );
            assert_eq!(
                encoded_len(&doc, format, expected.len() - 1),
                Err(RenderError::TooLarge)
            );
            let mut output = vec![0; expected.len()];
            assert_eq!(render_into(&doc, format, &mut output), Ok(expected.len()));
            assert_eq!(output, expected);
        }
        assert_eq!(
            encoded_len(&doc, Format::Jsonp("bad()"), usize::MAX),
            Err(RenderError::InvalidCallback)
        );
    }

    #[test]
    fn nested_values_match_existing_wire_formats() {
        let docs = [
            Value::Null,
            Value::List(vec![Value::Null, Value::List(vec![Value::Int(7)])]),
            Value::map(vec![
                ("empty", Value::List(vec![Value::Null])),
                ("value", Value::Bool(false)),
                (
                    "child",
                    Value::map(vec![("value", Value::str_value("<&\"é"))]),
                ),
                (
                    "items",
                    Value::List(vec![Value::Bool(true), Value::Int(i64::MIN)]),
                ),
            ]),
        ];
        for doc in &docs {
            for (format, expected) in [
                (Format::Json, render_json(doc)),
                (
                    Format::Jsonp("ns.cb[0]"),
                    render_jsonp(doc, Some("ns.cb[0]")).0,
                ),
                (Format::Xml, render_xml(doc)),
            ] {
                let len = encoded_len(doc, format, usize::MAX).unwrap();
                assert_eq!(len, expected.len());
                let mut short = vec![0x55; len - 1];
                assert_eq!(
                    render_into(doc, format, &mut short),
                    Err(RenderError::TooLarge)
                );
                assert!(short.iter().all(|&byte| byte == 0x55));
                let mut extra = vec![0x55; len + 1];
                assert_eq!(render_into(doc, format, &mut extra), Ok(len));
                assert_eq!(&extra[..len], expected);
                assert_eq!(extra[len], 0x55);
            }
        }
    }

    #[test]
    fn malformed_inputs_are_rejected_before_writing() {
        let doc = Value::map(vec![(
            "item",
            Value::List(vec![Value::List(vec![Value::Int(1)])]),
        )]);
        let mut out = [0x55; 256];
        assert_eq!(
            encoded_len(&doc, Format::Xml, usize::MAX),
            Err(RenderError::TooLarge)
        );
        assert_eq!(
            render_into(&doc, Format::Xml, &mut out),
            Err(RenderError::TooLarge)
        );
        for callback in ["", "cb()", "foo[0]bar", "foo[\"unterminated]"] {
            assert_eq!(
                render_into(&doc, Format::Jsonp(callback), &mut out),
                Err(RenderError::InvalidCallback)
            );
        }
        assert!(out.iter().all(|&byte| byte == 0x55));
        // Nested lists are valid in JSON even though the legacy XML renderer
        // cannot represent a list nested directly in a map's list.
        assert_eq!(
            encoded_len(&doc, Format::Json, usize::MAX),
            Ok(render_json(&doc).len())
        );
    }

    #[test]
    fn checked_count_overflow_is_rejected() {
        let mut sink = Sink {
            bytes: None,
            len: usize::MAX,
            limit: usize::MAX,
        };
        assert!(sink.write_str("x").is_err());
    }
}
