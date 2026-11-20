//! Worker conversion of completed bodies and fixed staging rejections.

use nntune_subsonic::envelope::{JSON_CONTENT_TYPE, XML_CONTENT_TYPE};
use nntune_subsonic::staging_rejection::{
    fixed_staging_rejection, RejectionFormat, StagingRejection,
};
use worker::{Headers, Response, Result as WorkerResult};

use super::{DocumentError, StagingDocument};

const METHOD_REFUSED: &[u8] = br#"<subsonic-response status="failed" version="1.16.1"><error code="0" message="Method not allowed."/></subsonic-response>"#;
const UNREADABLE: &[u8] = br#"<subsonic-response status="failed" version="1.16.1"><error code="10" message="Unable to read request body."/></subsonic-response>"#;
const UNSUPPORTED_ROUTE: &[u8] = br#"<subsonic-response status="failed" version="1.16.1"><error code="70" message="Endpoint not supported in staging."/></subsonic-response>"#;
const INVALID_CALLBACK: &[u8] = br#"{"subsonic-response":{"status":"failed","version":"1.16.1","error":{"code":10,"message":"Required parameter is missing."}}}"#;

fn build_response(status: u16, content_type: &str, bytes: Vec<u8>) -> WorkerResult<Response> {
    let headers = Headers::new();
    headers.set("Content-Type", content_type)?;
    headers.set("Cache-Control", "private, no-store")?;
    Ok(Response::from_bytes(bytes)?
        .with_status(status)
        .with_headers(headers))
}

pub fn respond_unsupported_method() -> WorkerResult<Response> {
    let mut response = build_response(405, XML_CONTENT_TYPE, METHOD_REFUSED.to_vec())?;
    response.headers_mut().set("Allow", "GET, POST")?;
    Ok(response)
}

pub fn respond_unreadable() -> WorkerResult<Response> {
    build_response(200, XML_CONTENT_TYPE, UNREADABLE.to_vec())
}

pub fn finish_document(
    document: Result<Option<StagingDocument>, DocumentError>,
) -> WorkerResult<Response> {
    match document {
        Ok(Some(doc)) => build_response(200, doc.content_type, doc.bytes),
        Ok(None) => build_response(404, XML_CONTENT_TYPE, UNSUPPORTED_ROUTE.to_vec()),
        Err(DocumentError::Rejected(reason, format)) => respond_fixed_rejection(reason, format),
        Err(DocumentError::InvalidCallback) => {
            build_response(200, JSON_CONTENT_TYPE, INVALID_CALLBACK.to_vec())
        }
    }
}

/// Fixed 413/503 responses have no request text, even on a JSONP request.
/// Use the default XML format unless a supported selector is already safe.
pub fn respond_fixed_rejection(
    reason: StagingRejection,
    format: RejectionFormat,
) -> WorkerResult<Response> {
    let fixed = fixed_staging_rejection(reason, format);
    let headers = Headers::new();
    headers.set("Content-Type", fixed.content_type)?;
    headers.set("Cache-Control", "private, no-store")?;
    if let Some(retry_after) = fixed.retry_after {
        headers.set("Retry-After", retry_after)?;
    }
    Ok(Response::from_bytes(fixed.body.to_vec())?
        .with_status(fixed.status)
        .with_headers(headers))
}

#[cfg(feature = "staging-body-wasm-test")]
mod wasm_test {
    use super::*;
    use crate::subsonic_staging_response::prepare;
    use nntune_subsonic::capped_render::Format;
    use nntune_subsonic::construction_budget::ConstructionBudget;
    use nntune_subsonic::value::Value;
    use worker::wasm_bindgen::{self, prelude::*};
    use worker::web_sys;

    #[wasm_bindgen]
    pub fn staging_rejection_test(busy: bool, jsonp: bool) -> Result<web_sys::Response, JsValue> {
        let format = if jsonp {
            Format::Jsonp("safe")
        } else {
            Format::Xml
        };
        let graph = Value::Map(Vec::new());
        let refused = prepare(
            &mut ConstructionBudget::new(0),
            &graph,
            format,
            if busy { 1024 } else { 1 },
        );
        assert!(refused.is_err());
        finish_document(refused.map(Some))
            .map(Into::into)
            .map_err(|_| JsValue::NULL)
    }
}
