use nntune_subsonic::capped_render::{encoded_len, render_into, Format, RenderError};
use nntune_subsonic::value::Value;

fn wraps(depth: usize, mut doc: Value, wrap: impl Fn(Value) -> Value) -> Value {
    for _ in 0..depth {
        doc = wrap(doc);
    }
    doc
}

fn rejects_before_writing(doc: &Value, format: Format<'_>) {
    let mut output = [0xa5; 4_096];
    assert_eq!(
        encoded_len(doc, format, usize::MAX),
        Err(RenderError::TooLarge)
    );
    assert_eq!(
        render_into(doc, format, &mut output),
        Err(RenderError::TooLarge)
    );
    assert_eq!(output, [0xa5; 4_096]);
}

#[test]
fn cap_counts_depth_even_when_xml_list_wrappers_write_no_bytes() {
    for format in [Format::Json, Format::Jsonp("cb"), Format::Xml] {
        let wrap = |doc| Value::List(vec![doc]);
        let doc = wraps(64, Value::Int(1), wrap);
        let len = encoded_len(&doc, format, 4_096).unwrap();
        assert_eq!(render_into(&doc, format, &mut vec![0; len]), Ok(len));
        rejects_before_writing(&wrap(doc), format);
    }
}

#[test]
fn xml_map_lists_charge_the_list_and_its_entry() {
    let wrap = |doc| Value::map(vec![("item", Value::List(vec![doc]))]);
    let doc = wraps(32, Value::Int(1), wrap);
    assert!(encoded_len(&doc, Format::Xml, 4_096).is_ok());
    rejects_before_writing(&wrap(doc), Format::Xml);
}

#[test]
fn xml_counts_empty_and_all_null_map_lists_at_the_limit() {
    for list in [vec![], vec![Value::Null]] {
        let leaf = Value::map(vec![("item", Value::List(list))]);
        let wrap = |doc| Value::map(vec![("child", doc)]);
        let doc = wraps(63, leaf, wrap);
        assert!(encoded_len(&doc, Format::Xml, 4_096).is_ok());
        rejects_before_writing(&wrap(doc), Format::Xml);
    }
}

#[test]
fn xml_requires_one_root_after_flattening_top_level_lists() {
    let empty = || Value::List(vec![]);
    for doc in [
        empty(),
        Value::List(vec![Value::Int(1), Value::Int(2)]),
        Value::List(vec![Value::Null, Value::List(vec![Value::Int(7)])]),
    ] {
        rejects_before_writing(&doc, Format::Xml);
    }
    let doc = Value::List(vec![empty(), Value::List(vec![Value::Int(1)]), empty()]);
    let expected =
        b"<?xml version=\"1.0\" encoding=\"UTF-8\"?><subsonic-response>1</subsonic-response>";
    let mut output = [0; 128];
    let len = render_into(&doc, Format::Xml, &mut output).unwrap();
    assert_eq!(&output[..len], expected);
}
