//! Inert preparation of bounded staging documents and Worker responses.

mod document;
#[cfg(target_arch = "wasm32")]
mod worker_response;

pub use document::{prepare, DocumentError, StagingDocument, DOCUMENT_BYTES};
#[cfg(target_arch = "wasm32")]
pub use worker_response::{
    finish_document, respond_fixed_rejection, respond_unreadable, respond_unsupported_method,
};
