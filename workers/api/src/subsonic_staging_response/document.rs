//! Prepare a complete response body on the existing construction ledger.

use nntune_subsonic::capped_render::{self, Format, RenderError};
use nntune_subsonic::construction_budget::ConstructionBudget;
use nntune_subsonic::envelope::{JSONP_CONTENT_TYPE, JSON_CONTENT_TYPE, XML_CONTENT_TYPE};
use nntune_subsonic::staging_rejection::{RejectionFormat, StagingRejection};
use nntune_subsonic::value::Value;

pub const DOCUMENT_BYTES: usize = nntune_subsonic::MAX_PRODUCTION_RESPONSE_BYTES;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DocumentError {
    Rejected(StagingRejection, RejectionFormat),
    InvalidCallback,
}

#[derive(Debug, PartialEq, Eq)]
pub struct StagingDocument {
    pub(super) content_type: &'static str,
    pub(super) bytes: Vec<u8>,
}

/// Render only a graph already charged during construction. The caller
/// retains the permit through Worker response preparation.
pub fn prepare(
    budget: &mut ConstructionBudget,
    graph: &Value,
    format: Format<'_>,
    limit: usize,
) -> Result<StagingDocument, DocumentError> {
    let (content_type, rejection_format) = match format {
        Format::Json => (JSON_CONTENT_TYPE, RejectionFormat::Json),
        Format::Jsonp(_) => (JSONP_CONTENT_TYPE, RejectionFormat::Json),
        Format::Xml => (XML_CONTENT_TYPE, RejectionFormat::Xml),
    };
    let classify = |error| match error {
        RenderError::TooLarge => DocumentError::Rejected(
            StagingRejection::RequestOrDocumentTooLarge,
            rejection_format,
        ),
        RenderError::InvalidCallback => DocumentError::InvalidCallback,
    };
    let len =
        capped_render::encoded_len(graph, format, limit.min(DOCUMENT_BYTES)).map_err(classify)?;
    let mut bytes = budget
        .allocate_exact(len)
        .map_err(|_| DocumentError::Rejected(StagingRejection::BusyOrCapacity, rejection_format))?;
    bytes.resize(len, 0);
    capped_render::render_into(graph, format, &mut bytes).map_err(classify)?;
    Ok(StagingDocument {
        content_type,
        bytes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalogue::ArtistDbRow;
    use crate::subsonic_dispatch::snapshot_order::OrderWork;
    use crate::subsonic_snapshot_plan::build_owned_bounded_snapshot;

    fn graph(budget: &mut ConstructionBudget) -> Value {
        let rows = [ArtistDbRow {
            mbid: "a".into(),
            name: "<&é".into(),
            sort_name: "name".into(),
            artist_type: None,
            country: None,
        }];
        build_owned_bounded_snapshot(
            budget,
            &rows,
            &[],
            &[],
            OrderWork {
                comparisons: 0,
                artist_row_probes: 0,
            },
            8 * 1024,
            8 * 1024 * 1024,
        )
        .unwrap();
        let title = Value::charged_str(budget, &rows[0].name).unwrap();
        Value::charged_map(budget, [("title", title)]).unwrap()
    }

    #[test]
    fn configured_limit_cannot_raise_the_document_ceiling() {
        for (format, rejection) in [
            (Format::Json, RejectionFormat::Json),
            (Format::Xml, RejectionFormat::Xml),
            (Format::Jsonp("music.done"), RejectionFormat::Json),
        ] {
            let overhead =
                capped_render::encoded_len(&Value::Str("a".into()), format, DOCUMENT_BYTES)
                    .unwrap()
                    - 1;
            for extra in [0, 1] {
                let text = "a".repeat(DOCUMENT_BYTES - overhead + extra);
                let mut budget = ConstructionBudget::new(usize::MAX);
                let doc = Value::charged_str(&mut budget, &text).unwrap();
                let carried = budget.charged();
                let result = prepare(&mut budget, &doc, format, usize::MAX);
                if extra == 0 {
                    assert_eq!(result.unwrap().bytes.len(), DOCUMENT_BYTES);
                } else {
                    assert_eq!(
                        result,
                        Err(DocumentError::Rejected(
                            StagingRejection::RequestOrDocumentTooLarge,
                            rejection,
                        ))
                    );
                    assert_eq!(budget.charged(), carried);
                }
            }
        }
    }

    #[test]
    fn output_uses_the_existing_snapshot_and_graph_ledger() {
        let mut budget = ConstructionBudget::new(usize::MAX);
        graph(&mut budget);
        for (format, rejection, content_type, expected) in [
            (
                Format::Json,
                RejectionFormat::Json,
                JSON_CONTENT_TYPE,
                r#"{"subsonic-response":{"title":"<&é"}}"#,
            ),
            (
                Format::Xml,
                RejectionFormat::Xml,
                XML_CONTENT_TYPE,
                r#"<?xml version="1.0" encoding="UTF-8"?><subsonic-response title="&lt;&amp;é"/>"#,
            ),
            (
                Format::Jsonp("music.done"),
                RejectionFormat::Json,
                JSONP_CONTENT_TYPE,
                r#"/**/music.done({"subsonic-response":{"title":"<&é"}});"#,
            ),
        ] {
            let len = expected.len();
            let mut charges = ConstructionBudget::new(usize::MAX);
            charges.charge(len, 1, 1).unwrap();
            let total = budget.charged() + charges.charged();
            for capacity in [total - 1, total, total + 1] {
                let mut carried = ConstructionBudget::new(capacity);
                let doc = graph(&mut carried);
                assert_eq!(
                    prepare(&mut carried, &doc, format, len - 1),
                    Err(DocumentError::Rejected(
                        StagingRejection::RequestOrDocumentTooLarge,
                        rejection,
                    ))
                );
                assert_eq!(carried.charged(), budget.charged());
                let result = prepare(&mut carried, &doc, format, len);
                if capacity < total {
                    assert_eq!(
                        result,
                        Err(DocumentError::Rejected(
                            StagingRejection::BusyOrCapacity,
                            rejection
                        ))
                    );
                } else {
                    let doc = result.unwrap();
                    assert_eq!(doc.content_type, content_type);
                    assert_eq!(doc.bytes.as_slice(), expected.as_bytes());
                    assert_eq!(doc.bytes.capacity(), len);
                    assert_eq!(carried.charged(), total);
                }
            }
        }
    }
}
