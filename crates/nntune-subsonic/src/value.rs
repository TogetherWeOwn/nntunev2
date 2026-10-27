//! Payload value model + serializers, ported from v1
//! `App\Http\Responses\Subsonic\SubsonicResponse`.
//!
//! Rules (v1 `serializeTo` / `stripNulls`, kept exactly):
//! - maps: scalar entries become attributes, sub-maps become child elements,
//!   lists become repeated child elements named after the key, a `value` entry
//!   becomes element text, bools render `true`/`false`, nulls are stripped.
//! - JSON keeps native types and strips nulls; output is compact.

use std::fmt::Write as _;

/// Ordered map entry list; insertion order is the wire order (as in v1).
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Int(i64),
    Str(String),
    List(Vec<Value>),
    Map(Vec<(String, Value)>),
}

impl Value {
    pub fn map(entries: Vec<(&str, Value)>) -> Value {
        Value::Map(
            entries
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect(),
        )
    }

    pub fn str_value(s: impl Into<String>) -> Value {
        Value::Str(s.into())
    }

    pub fn int_value(n: i64) -> Value {
        Value::Int(n)
    }

    pub fn bool_value(b: bool) -> Value {
        Value::Bool(b)
    }

    /// JSON body, compact, nulls stripped.
    pub fn to_json(&self) -> String {
        let mut out = String::new();
        write_json(self, &mut out);
        out
    }

    /// XML element `<name ...attrs>children</name>`, or self-closed when empty.
    pub fn to_xml_element(&self, name: &str) -> String {
        let mut out = String::new();
        write_xml_element(self, name, &mut out);
        out
    }
}

fn write_json(v: &Value, out: &mut String) {
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Int(n) => out.push_str(&n.to_string()),
        Value::Str(s) => write_json_string(s, out),
        Value::List(items) => {
            out.push('[');
            let mut first = true;
            for item in items {
                if matches!(item, Value::Null) {
                    continue;
                }
                if !first {
                    out.push(',');
                }
                first = false;
                write_json(item, out);
            }
            out.push(']');
        }
        Value::Map(entries) => {
            out.push('{');
            let mut first = true;
            for (k, val) in entries {
                if matches!(val, Value::Null) {
                    continue;
                }
                if !first {
                    out.push(',');
                }
                first = false;
                write_json_string(k, out);
                out.push(':');
                write_json(val, out);
            }
            out.push('}');
        }
    }
}

fn write_json_string(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

fn write_xml_element(v: &Value, name: &str, out: &mut String) {
    match v {
        Value::Map(entries) => {
            let mut attrs = String::new();
            let mut text: Option<&str> = None;
            let mut children: Vec<(&str, &Value)> = Vec::new();
            for (k, val) in entries {
                match val {
                    Value::Null => {}
                    Value::Bool(_) | Value::Int(_) | Value::Str(_) => {
                        if k == "value" {
                            if let Value::Str(s) = val {
                                text = Some(s);
                            } else {
                                // Non-string `value` cannot happen from our builders;
                                // fall back to attribute form rather than dropping data.
                                attrs.push(' ');
                                attrs.push_str(k);
                                attrs.push_str("=\"");
                                write_xml_scalar(val, &mut attrs);
                                attrs.push('"');
                            }
                        } else {
                            attrs.push(' ');
                            attrs.push_str(k);
                            attrs.push_str("=\"");
                            write_xml_scalar(val, &mut attrs);
                            attrs.push('"');
                        }
                    }
                    Value::List(items) => {
                        for item in items {
                            if matches!(item, Value::Null) {
                                continue;
                            }
                            children.push((k, item));
                        }
                    }
                    Value::Map(_) => children.push((k, val)),
                }
            }
            if children.is_empty() && text.is_none() {
                let _ = write!(out, "<{name}{attrs}/>");
                return;
            }
            let _ = write!(out, "<{name}{attrs}>");
            if let Some(t) = text {
                write_xml_escaped(t, out);
            }
            for (child_name, child) in children {
                match child {
                    Value::List(_) => unreachable!("lists are flattened above"),
                    Value::Map(_) => write_xml_element(child, child_name, out),
                    _ => {
                        // Scalar list item: `<key>text</key>` (v1 `value` rule).
                        out.push('<');
                        out.push_str(child_name);
                        out.push('>');
                        write_xml_scalar(child, out);
                        out.push_str("</");
                        out.push_str(child_name);
                        out.push('>');
                    }
                }
            }
            let _ = write!(out, "</{name}>");
        }
        // A bare list/map mismatch at the top level cannot happen from our
        // builders; render defensively.
        Value::List(items) => {
            for item in items {
                write_xml_element(item, name, out);
            }
        }
        scalar => {
            out.push('<');
            out.push_str(name);
            out.push('>');
            write_xml_scalar(scalar, out);
            out.push_str("</");
            out.push_str(name);
            out.push('>');
        }
    }
}

fn write_xml_scalar(v: &Value, out: &mut String) {
    match v {
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Int(n) => out.push_str(&n.to_string()),
        Value::Str(s) => write_xml_escaped(s, out),
        Value::Null => {}
        Value::Map(_) | Value::List(_) => {}
    }
}

fn write_xml_escaped(s: &str, out: &mut String) {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            c => out.push(c),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xml_matches_v1_shape() {
        let v = Value::map(vec![
            ("status", Value::str_value("ok")),
            ("openSubsonic", Value::bool_value(true)),
            (
                "openSubsonicExtensions",
                Value::List(vec![Value::map(vec![
                    ("name", Value::str_value("songLyrics")),
                    ("versions", Value::List(vec![Value::int_value(1)])),
                ])]),
            ),
            (
                "genre",
                Value::List(vec![Value::map(vec![
                    ("songCount", Value::int_value(1)),
                    ("value", Value::str_value("Rock")),
                ])]),
            ),
            ("nothing", Value::Null),
        ]);
        assert_eq!(
            v.to_xml_element("subsonic-response"),
            "<subsonic-response status=\"ok\" openSubsonic=\"true\">\
             <openSubsonicExtensions name=\"songLyrics\"><versions>1</versions></openSubsonicExtensions>\
             <genre songCount=\"1\">Rock</genre></subsonic-response>"
        );
    }

    #[test]
    fn json_strips_nulls_and_keeps_types() {
        let v = Value::map(vec![
            ("a", Value::int_value(1)),
            ("b", Value::Null),
            ("c", Value::bool_value(false)),
        ]);
        assert_eq!(v.to_json(), "{\"a\":1,\"c\":false}");
    }
}
