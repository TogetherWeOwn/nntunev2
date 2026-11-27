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
    envelope::{JSONP_CONTENT_TYPE, JSON_CONTENT_TYPE, XML_CONTENT_TYPE},
    staging_rejection::{RejectionFormat, StagingRejection},
};

#[cfg(any(test, target_arch = "wasm32"))]
impl StagingDocument {
    fn validate_response(&self) -> Result<RejectionFormat, DocumentError> {
        let unavailable = |format| DocumentError::Rejected(StagingRejection::Unavailable, format);
        let format = match self.content_type {
            XML_CONTENT_TYPE => RejectionFormat::Xml,
            JSON_CONTENT_TYPE | JSONP_CONTENT_TYPE => RejectionFormat::Json,
            _ => return Err(unavailable(RejectionFormat::Xml)),
        };
        if self.bytes.is_empty() {
            return Err(unavailable(format));
        }
        if self.bytes.len() > DOCUMENT_BYTES {
            return Err(DocumentError::Rejected(
                StagingRejection::RequestOrDocumentTooLarge,
                format,
            ));
        }
        Ok(format)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_documents_cannot_reach_worker_response_construction() {
        use RejectionFormat::{Json, Xml};
        use StagingRejection::{RequestOrDocumentTooLarge, Unavailable};
        for (content_type, size, expected) in [
            (
                "text/html",
                1,
                Err(DocumentError::Rejected(Unavailable, Xml)),
            ),
            (
                JSON_CONTENT_TYPE,
                0,
                Err(DocumentError::Rejected(Unavailable, Json)),
            ),
            (JSONP_CONTENT_TYPE, DOCUMENT_BYTES, Ok(Json)),
            (
                JSONP_CONTENT_TYPE,
                DOCUMENT_BYTES + 1,
                Err(DocumentError::Rejected(RequestOrDocumentTooLarge, Json)),
            ),
        ] {
            let doc = StagingDocument {
                content_type,
                bytes: vec![b'x'; size],
            };
            assert_eq!(doc.validate_response(), expected);
        }
    }
}
