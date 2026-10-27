//! Response envelope, ported from v1 `SubsonicResponse`.
//!
//! Every Subsonic document is HTTP 200 with `status="ok"` (payload merged
//! into the envelope) or `status="failed"` (plus an `error` child). The wire
//! format follows `f`: `json`, `jsonp`, anything else XML. `type` is `nntune`
//! (v1 said `koel` from its upstream heritage; see ADR 0001).

use crate::value::Value;

pub const API_VERSION: &str = "1.16.1";
pub const SERVER_TYPE: &str = "nntune";
pub const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

pub const XML_CONTENT_TYPE: &str = "application/xml";
pub const JSON_CONTENT_TYPE: &str = "application/json";
// Symfony `JsonResponse::update` serves JSONP as `text/javascript` with a
// `/**/` prefix (content-sniffing guard, e.g. Rosetta Flash), never
// `application/javascript`.
pub const JSONP_CONTENT_TYPE: &str = "text/javascript";

/// Standard Subsonic error codes used by this port.
pub mod code {
    pub const GENERIC: i64 = 0;
    pub const REQUIRED_PARAM_MISSING: i64 = 10;
    pub const WRONG_CREDENTIALS: i64 = 40;
    pub const CONFLICTING_AUTH: i64 = 43;
    pub const INVALID_API_KEY: i64 = 44;
    pub const NOT_AUTHORIZED: i64 = 50;
    pub const NOT_FOUND: i64 = 70;
}

fn base_envelope(error: Option<(i64, &str)>) -> Vec<(String, Value)> {
    let mut entries = vec![
        (
            "status".to_string(),
            Value::str_value(if error.is_none() { "ok" } else { "failed" }),
        ),
        ("version".to_string(), Value::str_value(API_VERSION)),
        ("type".to_string(), Value::str_value(SERVER_TYPE)),
        (
            "serverVersion".to_string(),
            Value::str_value(SERVER_VERSION),
        ),
        ("openSubsonic".to_string(), Value::bool_value(true)),
    ];
    if let Some((error_code, message)) = error {
        entries.push((
            "error".to_string(),
            Value::map(vec![
                ("code", Value::int_value(error_code)),
                ("message", Value::str_value(message)),
            ]),
        ));
    }
    entries
}

/// Successful document: payload entries merged into the envelope, in order.
pub fn ok_document(payload: Vec<(String, Value)>) -> Value {
    let mut entries = base_envelope(None);
    entries.extend(payload);
    Value::Map(entries)
}

/// Failed document with a Subsonic error code/message.
pub fn error_document(error_code: i64, message: &str) -> Value {
    Value::Map(base_envelope(Some((error_code, message))))
}

/// Render a document for `f=json`.
pub fn render_json(doc: &Value) -> Vec<u8> {
    format!("{{\"subsonic-response\":{}}}", doc.to_json()).into_bytes()
}

/// Render a document for `f=jsonp`: `/**/{callback}({...});` as
/// `text/javascript`, exactly like v1's Laravel `->jsonp()` (Symfony
/// `JsonResponse::update`). A missing/empty/invalid callback is itself
/// error 10 JSON.
pub fn render_jsonp(doc: &Value, callback: Option<&str>) -> (Vec<u8>, &'static str) {
    match callback {
        Some(cb) if is_valid_jsonp_callback(cb) => (
            format!("/**/{cb}({{\"subsonic-response\":{}}});", doc.to_json()).into_bytes(),
            JSONP_CONTENT_TYPE,
        ),
        _ => (
            render_json(&error_document(
                code::REQUIRED_PARAM_MISSING,
                "Required parameter is missing.",
            )),
            JSON_CONTENT_TYPE,
        ),
    }
}

/// Symfony `JsonResponse::setCallback` equivalent, hand-rolled (this crate is
/// dependency-free): each dot-separated part must match
/// `^[$_\p{L}][$_\p{L}\p{Mn}\p{Mc}\p{Nd}\p{Pc}\u{200C}\u{200D}]*(?:\[...])*?$`
/// where each `[...]` holds a double-quoted string, a single-quoted string
/// (both with backslash escapes), or ASCII digits — and no part may be a
/// reserved word. Anything else is rejected so the API origin can never
/// serve attacker-chosen script.
///
/// Two deliberate approximations, both fail-closed (a rejected callback
/// falls back to JSON error 10, never to script): identifier characters use
/// `is_alphabetic`/`is_numeric` instead of exact Unicode general categories
/// (a superset for letters, so exotic-but-valid identifiers still pass;
/// non-ASCII marks/digits outside those classes are rejected), and callbacks
/// over 128 bytes are rejected as a DoS guard (Symfony has no cap, but real
/// clients send short names).
pub fn is_valid_jsonp_callback(cb: &str) -> bool {
    if cb.is_empty() || cb.len() > 128 {
        return false;
    }
    cb.split('.').all(is_valid_jsonp_part) && !cb.split('.').any(is_reserved_word)
}

/// Reserved words are checked per whole dot-separated part, exactly like
/// Symfony's `in_array($part, $reserved, true)` — bare `this` is rejected
/// while `this.ok` passes the word check (and then the identifier check).
fn is_valid_jsonp_part(part: &str) -> bool {
    let base_end = part.find('[').unwrap_or(part.len());
    let (base, mut rest) = part.split_at(base_end);
    if !is_js_identifier(base) {
        return false;
    }
    // Quote-aware indexer scan, mirroring Symfony's
    // `\[(?:"(?:\\\.|[^"\\\])*"|'(?:\\\.|[^'\\\])*'|\d+)\]` (a `]` inside
    // quotes does not close the indexer).
    while let Some(after_open) = rest.strip_prefix('[') {
        let after_body = match indexer_body(after_open) {
            Some(after_body) => after_body,
            None => return false,
        };
        match after_body.strip_prefix(']') {
            Some(after_close) => rest = after_close,
            None => return false,
        }
    }
    // The whole part must be consumed (Symfony's `$` anchor): `foo[0]bar`
    // is not a valid callback.
    rest.is_empty()
}

/// Remainder after one indexer body (the text between `[` and `]`):
/// a quoted string with backslash escapes, or ASCII digits (Symfony `\d`).
fn indexer_body(rest: &str) -> Option<&str> {
    if let Some(after_quote) = rest.strip_prefix('"') {
        return scan_quoted(after_quote, '"');
    }
    if let Some(after_quote) = rest.strip_prefix('\'') {
        return scan_quoted(after_quote, '\'');
    }
    let end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    if end == 0 {
        return None;
    }
    Some(&rest[end..])
}

fn scan_quoted(rest: &str, quote: char) -> Option<&str> {
    let mut chars = rest.char_indices();
    while let Some((i, c)) = chars.next() {
        if c == '\\' {
            chars.next()?;
        } else if c == quote {
            return Some(&rest[i + 1..]);
        }
    }
    None
}

fn is_js_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c == '$' || c == '_' || c.is_alphabetic() => {}
        _ => return false,
    }
    for c in chars {
        if !(c == '$'
            || c == '_'
            || c == '\u{200C}'
            || c == '\u{200D}'
            || c.is_alphabetic()
            || c.is_numeric())
        {
            return false;
        }
    }
    true
}

/// Symfony's reserved-word list verbatim (`JsonResponse::setCallback`).
fn is_reserved_word(part: &str) -> bool {
    matches!(
        part,
        "break"
            | "do"
            | "instanceof"
            | "typeof"
            | "case"
            | "else"
            | "new"
            | "var"
            | "catch"
            | "finally"
            | "return"
            | "void"
            | "continue"
            | "for"
            | "switch"
            | "while"
            | "debugger"
            | "function"
            | "this"
            | "with"
            | "default"
            | "if"
            | "throw"
            | "delete"
            | "in"
            | "try"
            | "class"
            | "enum"
            | "extends"
            | "super"
            | "const"
            | "export"
            | "import"
            | "implements"
            | "let"
            | "private"
            | "public"
            | "yield"
            | "interface"
            | "package"
            | "protected"
            | "static"
            | "null"
            | "true"
            | "false"
    )
}

/// Render a document as XML (the default `f`).
pub fn render_xml(doc: &Value) -> Vec<u8> {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>{}",
        doc.to_xml_element("subsonic-response")
    )
    .into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ok_envelope_orders_attributes_like_v1() {
        let body = String::from_utf8(render_xml(&ok_document(vec![]))).unwrap();
        assert_eq!(
            body,
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\
             <subsonic-response status=\"ok\" version=\"1.16.1\" type=\"nntune\" \
             serverVersion=\"0.1.0\" openSubsonic=\"true\"/>"
        );
    }

    #[test]
    fn jsonp_without_callback_falls_back_to_error_10() {
        let (body, ct) = render_jsonp(&ok_document(vec![]), None);
        assert_eq!(ct, JSON_CONTENT_TYPE);
        assert!(String::from_utf8(body).unwrap().contains("\"code\":10"));
    }

    #[test]
    fn jsonp_uses_symfony_framing() {
        let (body, ct) = render_jsonp(&ok_document(vec![]), Some("cb"));
        assert_eq!(ct, "text/javascript");
        let text = String::from_utf8(body).unwrap();
        assert!(text.starts_with("/**/cb({"), "{text}");
        assert!(text.ends_with("});"), "{text}");
    }

    #[test]
    fn jsonp_rejects_non_identifier_callbacks() {
        for bad in [
            "alert(document.domain);//",
            "alert(1)",
            "cb;evil()",
            "foo bar",
            "",
            "foo..bar",
            "123abc",
            "a-b",
            // Symfony rejects unquoted non-digit indexers and reserved words.
            "top[name]",
            "a[b-c]",
            "this",
            "function",
            "new",
            "foo[]",
            "foo[0",
            "foo[\"bar]",
            "foo[0]bar",
            "foo(bar)",
        ] {
            let (body, ct) = render_jsonp(&ok_document(vec![]), Some(bad));
            assert_eq!(ct, JSON_CONTENT_TYPE, "{bad}");
            assert!(
                String::from_utf8(body).unwrap().contains("\"code\":10"),
                "{bad}"
            );
        }
        for good in [
            "cb",
            "myApp.cb_1",
            "$jQuery.foo",
            "ns.cb[0].done",
            "foo[\"b-ar\"].ok",
            "foo['b'].ok",
            "foo[\"a]b\"].ok",
        ] {
            let (_, ct) = render_jsonp(&ok_document(vec![]), Some(good));
            assert_eq!(ct, JSONP_CONTENT_TYPE, "{good}");
        }
    }
}
