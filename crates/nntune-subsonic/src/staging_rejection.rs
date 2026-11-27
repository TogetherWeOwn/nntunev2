//! Fixed responses for the inert staging admission boundary.
//!
//! This does not perform admission or change existing protocol responses.
//! Callers must select a format only after safely parsing the request; an
//! unknown or unsafe selector uses the default. JSONP is deliberately JSON,
//! never executable script containing a request-supplied callback.

use crate::envelope::{code, API_VERSION, JSON_CONTENT_TYPE, XML_CONTENT_TYPE};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RejectionFormat {
    Json,
    Xml,
}

impl RejectionFormat {
    /// Map an already safely selected format to a non-executable response.
    pub fn from_safe_selector(selector: Option<&str>) -> Self {
        match selector {
            Some("json" | "jsonp") => Self::Json,
            _ => Self::Xml,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StagingRejection {
    RequestOrDocumentTooLarge,
    BusyOrCapacity,
    Unavailable,
}

/// A static body and headers; no request data or internal error is accepted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FixedRejectionResponse {
    pub status: u16,
    pub content_type: &'static str,
    pub body: &'static [u8],
    pub retry_after: Option<&'static str>,
}

macro_rules! fixed_bodies {
    ($version:literal, $error:literal; $($name:ident => $message:literal),+ $(,)?) => {
        const _: () = {
            const VERSION: &[u8] = $version.as_bytes();
            assert!(matches!(API_VERSION.as_bytes(), VERSION));
            assert!(code::GENERIC == $error);
        };
        $(
            const $name: (&[u8], &[u8]) = (
                concat!(r#"{"subsonic-response":{"status":"failed","version":""#, $version,
                    r#"","error":{"code":"#, $error, r#","message":""#, $message, r#""}}}"#).as_bytes(),
                concat!(r#"<subsonic-response status="failed" version=""#, $version,
                    r#""><error code=""#, $error, r#"" message=""#, $message, r#""/></subsonic-response>"#).as_bytes(),
            );
            const _: () = assert!($name.0.len() <= 1024 && $name.1.len() <= 1024);
        )+
    };
}

fixed_bodies!("1.16.1", 0;
    TOO_LARGE => "Request or document too large.",
    UNAVAILABLE => "Service unavailable.",
);

pub fn fixed_staging_rejection(
    rejection: StagingRejection,
    format: RejectionFormat,
) -> FixedRejectionResponse {
    let (status, retry_after, (json, xml)) = match rejection {
        StagingRejection::RequestOrDocumentTooLarge => (413, None, TOO_LARGE),
        StagingRejection::BusyOrCapacity => (503, Some("1"), UNAVAILABLE),
        StagingRejection::Unavailable => (503, None, UNAVAILABLE),
    };
    let (content_type, body) = match format {
        RejectionFormat::Json => (JSON_CONTENT_TYPE, json),
        RejectionFormat::Xml => (XML_CONTENT_TYPE, xml),
    };
    FixedRejectionResponse {
        status,
        content_type,
        body,
        retry_after,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn responses_are_fixed_bounded_and_have_expected_status_and_headers() {
        for (reason, status, retry_after) in [
            (StagingRejection::RequestOrDocumentTooLarge, 413, None),
            (StagingRejection::BusyOrCapacity, 503, Some("1")),
            (StagingRejection::Unavailable, 503, None),
        ] {
            for format in [RejectionFormat::Json, RejectionFormat::Xml] {
                let response = fixed_staging_rejection(reason, format);
                assert_eq!(response.status, status);
                assert_eq!(response.retry_after, retry_after);
                assert!(!response.body.is_empty() && response.body.len() <= 1024);
                assert_eq!(response, fixed_staging_rejection(reason, format));
                let text = std::str::from_utf8(response.body).unwrap();
                assert!(text.contains("failed"));
                assert!(text.contains(if status == 413 {
                    "too large"
                } else {
                    "unavailable"
                }));
            }
        }
    }

    #[test]
    fn safe_format_selection_never_reflects_request_data_or_emits_jsonp() {
        for selector in [
            None,
            Some("xml"),
            Some("unknown-secret"),
            Some("json"),
            Some("jsonp"),
        ] {
            let format = RejectionFormat::from_safe_selector(selector);
            let response = fixed_staging_rejection(StagingRejection::BusyOrCapacity, format);
            let expected = if matches!(selector, Some("json" | "jsonp")) {
                JSON_CONTENT_TYPE
            } else {
                XML_CONTENT_TYPE
            };
            assert_eq!(response.content_type, expected);
            let text = std::str::from_utf8(response.body).unwrap();
            for sensitive in [
                "unknown-secret",
                "password-secret",
                "token-secret",
                "callbackSecret",
            ] {
                assert!(!text.contains(sensitive));
            }
            assert!(!text.starts_with("/**/"));
        }
    }
}
