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
pub const JSONP_CONTENT_TYPE: &str = "application/javascript";

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

/// Render a document for `f=jsonp`. A missing/empty/invalid callback is
/// itself error 10 JSON, exactly like v1's `toJsonp` fallback (v1 goes
/// through Symfony `setCallback`, which rejects anything that is not a
/// dot-separated JS identifier path with optional `[...]` parts).
pub fn render_jsonp(doc: &Value, callback: Option<&str>) -> (Vec<u8>, &'static str) {
    match callback {
        Some(cb) if is_valid_jsonp_callback(cb) => (
            format!("{}({{\"subsonic-response\":{}}})", cb, doc.to_json()).into_bytes(),
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

/// Symfony `JsonResponse::setCallback` equivalent: dot-separated JS
/// identifiers (`foo`, `foo.bar`, `foo[0].bar`), nothing else. Anything with
/// parens, semicolons, slashes or whitespace is rejected so the API origin
/// can never serve attacker-chosen script.
pub fn is_valid_jsonp_callback(cb: &str) -> bool {
    if cb.is_empty() || cb.len() > 128 {
        return false;
    }
    for segment in cb.split('.') {
        if segment.is_empty() {
            return false;
        }
        // Strip trailing `[...]` indexers: `foo[0][bar]`.
        let mut rest = segment;
        let base_end = rest.find('[').unwrap_or(rest.len());
        if !is_js_identifier(&rest[..base_end]) {
            return false;
        }
        rest = &rest[base_end..];
        while !rest.is_empty() {
            if !rest.starts_with('[') {
                return false;
            }
            let close = match rest.find(']') {
                Some(i) => i,
                None => return false,
            };
            let inner = &rest[1..close];
            if inner.is_empty() {
                return false;
            }
            // Unquoted indexers allow alnum/`_`/`$`/`-`; quoted ones are
            // rejected to keep the pattern tight (v1 clients never need them).
            if !inner
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'$' || b == b'-')
            {
                return false;
            }
            rest = &rest[close + 1..];
        }
    }
    true
}

fn is_js_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' || c == '$' => {}
        _ => return false,
    }
    for c in chars {
        if !(c.is_ascii_alphanumeric() || c == '_' || c == '$') {
            return false;
        }
    }
    true
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
        ] {
            let (body, ct) = render_jsonp(&ok_document(vec![]), Some(bad));
            assert_eq!(ct, JSON_CONTENT_TYPE, "{bad}");
            assert!(
                String::from_utf8(body).unwrap().contains("\"code\":10"),
                "{bad}"
            );
        }
        for good in ["cb", "myApp.cb_1", "$jQuery.foo", "ns.cb[0].done"] {
            let (_, ct) = render_jsonp(&ok_document(vec![]), Some(good));
            assert_eq!(ct, JSONP_CONTENT_TYPE, "{good}");
        }
    }
}
