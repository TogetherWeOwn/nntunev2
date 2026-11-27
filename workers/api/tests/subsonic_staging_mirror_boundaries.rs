//! Native, in-memory checks against the Worker staging implementations.
use nntune_api::{catalogue, subsonic_dispatch, subsonic_snapshot_plan};
use nntune_subsonic::capped_render::Format;
use nntune_subsonic::construction_budget::ConstructionBudget;
use nntune_subsonic::staging_rejection::{RejectionFormat, StagingRejection};
use nntune_subsonic::value::Value;

#[path = "../src/subsonic_staging_response/document.rs"]
mod document;
#[path = "../src/subsonic_staging/fixture.rs"]
mod fixture;

#[test]
fn xml_refuses_forbidden_text_without_charging_an_output() {
    for text in ["control\u{1}", "noncharacter\u{fffe}", "nul\0"] {
        let mut budget = ConstructionBudget::new(4096);
        let title = Value::charged_str(&mut budget, text).unwrap();
        let graph = Value::charged_map(&mut budget, [("title", title)]).unwrap();
        let carried = budget.charged();
        assert_eq!(
            document::prepare(&mut budget, &graph, Format::Xml, 1024),
            Err(document::DocumentError::Rejected(
                StagingRejection::Unavailable,
                RejectionFormat::Xml
            ))
        );
        assert_eq!(budget.charged(), carried);
        assert!(document::prepare(&mut budget, &graph, Format::Json, 1024).is_ok());
    }
    let mut budget = ConstructionBudget::new(4096);
    let text = Value::charged_str(&mut budget, "雪 & 😀\t\n\r").unwrap();
    assert!(document::prepare(&mut budget, &text, Format::Xml, 1024).is_ok());
}

#[test]
fn fixture_rows_refuse_each_excess_without_returning_a_partial_catalogue() {
    for (boundary, exact_count) in [("rows", 1024), ("field", 1), ("text", 256)] {
        let rows = |suffix| {
            fixture::SyntheticCatalogue::from_trusted_id(&format!("{boundary}-{suffix}"))
                .unwrap()
                .rows(&mut ConstructionBudget::new(8 * 1024 * 1024))
        };
        let (artists, releases, tracks) = rows("exact").unwrap();
        assert_eq!(
            (artists.len(), releases.len(), tracks.len()),
            (1, 1, exact_count)
        );
        assert!(rows("over").is_err(), "{boundary}");
    }
}
