use nntune_subsonic::capped_render::{encoded_len, render_into, Format, RenderError};
use nntune_subsonic::construction_budget::{BudgetExceeded, ConstructionBudget};
use nntune_subsonic::construction_permit::CONSTRUCTION_SLOT;
use nntune_subsonic::envelope::{JSON_CONTENT_TYPE, XML_CONTENT_TYPE};
use nntune_subsonic::staging_rejection::{
    fixed_staging_rejection, RejectionFormat, StagingRejection,
};
use nntune_subsonic::value::Value;

#[test]
fn escaped_document_fits_only_at_the_exact_wire_and_construction_limits() {
    for (format, input, expected) in [
        (
            Format::Json,
            "é🎵\"\\\n\0\u{1f}",
            r#"{"subsonic-response":"é🎵\"\\\n\u0000\u001f"}"#,
        ),
        (
            Format::Jsonp("player.done"),
            "é🎵\"\\\n\0\u{1f}",
            r#"/**/player.done({"subsonic-response":"é🎵\"\\\n\u0000\u001f"});"#,
        ),
        (
            Format::Xml,
            "é🎵&<>\"",
            r#"<?xml version="1.0" encoding="UTF-8"?><subsonic-response>é🎵&amp;&lt;&gt;&quot;</subsonic-response>"#,
        ),
    ] {
        let wire_len = expected.len();
        // Both source strings occupy one 24-byte charged chunk. The output
        // adds four bytes before alignment and eight more for allocator slack.
        let construction_limit = 24 + (wire_len + 4).next_multiple_of(8) + 8;
        let mut budget = ConstructionBudget::new(construction_limit - 1);
        let doc = Value::charged_str(&mut budget, input).unwrap();
        assert_eq!(budget.charged(), 24);
        assert_eq!(encoded_len(&doc, format, wire_len), Ok(wire_len));
        for limit in [0, wire_len - 1] {
            assert_eq!(encoded_len(&doc, format, limit), Err(RenderError::TooLarge));
            let mut output = vec![0xA5; limit];
            assert_eq!(
                render_into(&doc, format, &mut output),
                Err(RenderError::TooLarge)
            );
            assert!(output.iter().all(|&byte| byte == 0xA5));
        }
        assert_eq!(budget.allocate_exact::<u8>(wire_len), Err(BudgetExceeded));
        assert_eq!(budget.charged(), 24);

        let mut budget = ConstructionBudget::new(construction_limit);
        let doc = Value::charged_str(&mut budget, input).unwrap();
        let mut output = budget.allocate_exact(wire_len).unwrap();
        output.resize(wire_len, 0);
        assert_eq!(render_into(&doc, format, &mut output), Ok(wire_len));
        assert_eq!(output, expected.as_bytes());
        assert_eq!(output.capacity(), wire_len);
        assert_eq!(budget.charged(), construction_limit);
        assert_eq!(budget.charge_payload_bytes(1), Err(BudgetExceeded));
    }
}

#[test]
fn zero_and_saturated_ledgers_reject_positive_charges() {
    let mut zero = ConstructionBudget::new(0);
    zero.restrict_remaining(usize::MAX);
    assert_eq!(zero.allocate_exact::<u8>(0).unwrap().capacity(), 0);
    assert!(zero.allocate_exact::<()>(usize::MAX).unwrap().is_empty());
    assert_eq!(zero.charge_payload_bytes(0), Ok(()));
    assert_eq!(zero.allocate_exact::<u8>(1), Err(BudgetExceeded));
    assert_eq!(zero.charged(), 0);

    let mut saturated = ConstructionBudget::new(usize::MAX);
    assert_eq!(saturated.charge(usize::MAX, 2, 1), Err(BudgetExceeded));
    assert_eq!(saturated.charge(usize::MAX, 1, 1), Err(BudgetExceeded));
    assert_eq!(saturated.charged(), 0);
    saturated.charge_payload_bytes(usize::MAX).unwrap();
    saturated.restrict_remaining(usize::MAX);
    assert_eq!(saturated.charge_payload_bytes(0), Ok(()));
    assert_eq!(saturated.charge_payload_bytes(1), Err(BudgetExceeded));
    assert_eq!(saturated.allocate_exact::<u8>(1), Err(BudgetExceeded));
    assert_eq!(saturated.charged(), usize::MAX);
}

#[test]
fn failed_vector_reservation_preserves_previous_charges() {
    let mut budget = ConstructionBudget::new(usize::MAX);
    let previous = isize::MAX as usize - 32;
    budget.charge_payload_bytes(previous).unwrap();
    // The debit fits, but Vec rejects allocations larger than isize::MAX
    // before calling the allocator.
    let impossible = isize::MAX as usize + 1;
    assert_eq!(budget.allocate_exact::<u8>(impossible), Err(BudgetExceeded));
    assert_eq!(budget.charged(), previous);
    budget.charge_payload_bytes(usize::MAX - previous).unwrap();
}

#[test]
fn shared_permit_releases_after_drop_render_error_and_unwind() {
    let permit = CONSTRUCTION_SLOT.try_acquire().unwrap();
    assert!(CONSTRUCTION_SLOT.try_acquire().is_none());
    drop(permit);

    fn refuse_document() -> Result<(), RenderError> {
        let _permit = CONSTRUCTION_SLOT.try_acquire().unwrap();
        encoded_len(&Value::Null, Format::Json, 0)?;
        Ok(())
    }

    assert_eq!(refuse_document(), Err(RenderError::TooLarge));
    let unwind = std::panic::catch_unwind(|| {
        let _permit = CONSTRUCTION_SLOT.try_acquire().unwrap();
        panic!("synthetic construction unwind");
    });
    assert!(unwind.is_err());
    assert!(CONSTRUCTION_SLOT.try_acquire().is_some());
}

#[test]
fn ceiling_rejections_are_bounded_and_jsonp_refusals_are_plain_json() {
    use StagingRejection::{BusyOrCapacity, RequestOrDocumentTooLarge, Unavailable};

    for (reason, status, retry_after) in [
        (RequestOrDocumentTooLarge, 413, None),
        (BusyOrCapacity, 503, Some("1")),
        (Unavailable, 503, None),
    ] {
        let json = fixed_staging_rejection(reason, RejectionFormat::Json);
        let xml = fixed_staging_rejection(reason, RejectionFormat::Xml);
        assert_eq!(json.content_type, JSON_CONTENT_TYPE);
        assert_eq!(xml.content_type, XML_CONTENT_TYPE);
        assert!(json
            .body
            .starts_with(br#"{"subsonic-response":{"status":"failed""#));
        assert!(xml
            .body
            .starts_with(br#"<subsonic-response status="failed""#));
        for response in [json, xml] {
            assert_eq!(
                (response.status, response.retry_after),
                (status, retry_after)
            );
            assert!(response.body.len() <= 1024);
        }
        assert_eq!(
            fixed_staging_rejection(reason, RejectionFormat::from_safe_selector(Some("jsonp"))),
            json
        );
        assert_eq!(
            fixed_staging_rejection(reason, RejectionFormat::from_safe_selector(Some("other"))),
            xml
        );
    }
}
