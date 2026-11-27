//! Inert preparation of bounded staging documents and Worker responses.

mod document;
#[cfg(target_arch = "wasm32")]
mod worker_response;

pub use document::{prepare, DocumentError, StagingDocument, DOCUMENT_BYTES};
#[cfg(all(target_arch = "wasm32", feature = "staging-body-wasm-test"))]
pub use worker_response::finish_streamed_document;
#[cfg(target_arch = "wasm32")]
pub use worker_response::{
    finish_document, respond_fixed_rejection, respond_unreadable, respond_unsupported_method,
};

#[cfg(any(test, target_arch = "wasm32"))]
use nntune_subsonic::{
    capped_render::Format, envelope::is_valid_jsonp_callback, staging_rejection::RejectionFormat,
    Params,
};

/// Resolve the representation before invoking the staging connector.
#[cfg(any(test, target_arch = "wasm32"))]
pub(crate) fn with_request_format<T>(
    params: &Params,
    open: impl FnOnce() -> T,
) -> Result<(Format<'_>, RejectionFormat, T), DocumentError> {
    let selector = params.get("f");
    let format = match selector {
        Some("json") => Format::Json,
        Some("jsonp") => Format::Jsonp(
            params
                .get("callback")
                .filter(|cb| is_valid_jsonp_callback(cb))
                .ok_or(DocumentError::InvalidCallback)?,
        ),
        _ => Format::Xml,
    };
    Ok((
        format,
        RejectionFormat::from_safe_selector(selector),
        open(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_jsonp_format_never_opens_the_app() {
        for query in ["f=jsonp", "f=jsonp&callback=invalid%20callback"] {
            let params = Params::parse(query);
            assert!(matches!(
                with_request_format(&params, || panic!("invalid callback reached connector")),
                Err(DocumentError::InvalidCallback)
            ));
        }
        let params = Params::parse("f=jsonp&callback=fixture.cb");
        assert!(matches!(
            with_request_format(&params, || 200),
            Ok((Format::Jsonp("fixture.cb"), RejectionFormat::Json, 200))
        ));
    }
}
