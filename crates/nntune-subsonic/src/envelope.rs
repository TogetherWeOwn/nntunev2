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
    pub const TOKEN_AUTH_UNSUPPORTED: i64 = 41;
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

/// Render a document for `f=jsonp`. A missing/empty callback is itself
/// error 10 JSON, exactly like v1's `toJsonp` fallback.
pub fn render_jsonp(doc: &Value, callback: Option<&str>) -> (Vec<u8>, &'static str) {
    match callback {
        Some(cb) if !cb.is_empty() => (
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
}
