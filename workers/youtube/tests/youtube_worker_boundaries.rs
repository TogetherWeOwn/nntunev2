use nntune_youtube::{client, gate, player, probe, routes, select};
use serde_json::{json, Value};

const VIDEO: &str = "abcdefghijk";
const STREAM: &str = "https://rr1---sn-fixture.googlevideo.com/videoplayback";

#[test]
fn client_requires_pinned_host_and_valid_inputs() {
    assert!(client::player_url(client::INNERTUBE_HOST, "fixture_key").is_some());
    assert!(client::player_url("example.invalid", "fixture_key").is_none());
    for key in ["", "fixture&extra=value", "fixture%20", "fixture\n"] {
        assert!(client::player_url(client::INNERTUBE_HOST, key).is_none());
    }
    assert!(client::player_body(&client::VISIONOS, "abcdefghij/").is_none());
    assert!(client::player_body(&client::VISIONOS, VIDEO).is_some());
}

#[test]
fn gate_uses_exact_counts_at_five_percent() {
    let total = u64::MAX - 15;
    assert!(gate::evaluate_gate(total / 20, total).go);
    assert!(!gate::evaluate_gate(total / 20 + 1, total).go);
    assert_eq!(gate::evaluate_gate(2, 1).failure_rate, 1.0);
    assert!(!gate::evaluate_gate(0, 0).go);
}

#[test]
fn mid_probe_requires_matching_partial_content_evidence() {
    let valid = "bytes 0-262143/524288";
    let head = probe::HEAD_BYTES;
    let range = probe::mid_range_from_head(206, valid, head);
    assert_eq!(range.as_deref(), Some("bytes=131072-393215"));
    assert!(probe::mid_range_from_head(200, valid, head).is_none());
    assert!(probe::mid_range_from_head(206, valid, head - 1).is_none());
    for range in [
        "bytes 1-262144/524288",
        "bytes 0-262143/*",
        "bytes 0-262143/100",
    ] {
        assert!(
            probe::mid_range_from_head(206, range, head).is_none(),
            "{range}"
        );
    }
}

fn response(status: Value, formats: Value) -> Value {
    json!({"playabilityStatus": {"status": status}, "streamingData": {"adaptiveFormats": formats}})
}

#[test]
fn player_requires_playability_and_valid_stream_metadata() {
    let formats = json!([false, {"url": "https://example.invalid/video"}, {
        "url": format!("{STREAM}?ip=203.0.113.1&ip=203.0.113.2")
    }]);
    for status in [json!("LOGIN_REQUIRED"), json!("ERROR"), Value::Null] {
        let outcome = player::parse_player_response(&response(status, formats.clone()));
        assert!(outcome.formats.is_empty());
    }
    let outcome = player::parse_player_response(&response(json!("OK"), formats));
    assert_eq!(outcome.formats.len(), 2);
    assert!(outcome.formats[0].url.is_none());
    assert!(outcome.formats[1].ip.is_none());
    assert!(player::query_param("https://example.invalid/#fragment?ip=1", "ip").is_none());
}

#[test]
fn audio_skips_unsupported_urls_and_mime_before_ranking() {
    let formats = json!([
        {"itag": 251, "mimeType": "video/webm; codecs=\"opus\"", "url": STREAM},
        {"itag": 251, "mimeType": "audio/webm; codecs=\"notopus\"", "url": STREAM},
        {"itag": 251, "mimeType": "audio/webm; codecs=\"opus\"", "url": STREAM, "cipher": {}},
        {"itag": 251, "mimeType": "audio/webm; codecs=\"opus\"", "url": format!("{STREAM}?n")},
        {"itag": 251, "mimeType": "audio/webm; codecs=\"opus\"", "url": format!("{STREAM}?%6e=x")},
        {"itag": 251, "mimeType": "audio/webm; codecs=\"opus\"", "url": format!("{STREAM}?sabr=0&sabr=1")},
        {"itag": 250, "mimeType": "audio/webm; codecs=\"opus\"", "url": STREAM}
    ]);
    let outcome = player::parse_player_response(&response(json!("OK"), formats));
    let selected = select::select_audio_format(&outcome.formats, select::AudioSource::Atv);
    assert_eq!(selected.map(|format| format.itag), Some(250));
}

#[test]
fn candidate_rejects_conflicting_ids_and_ambiguous_shelf_metadata() {
    let mut item = json!({
        "navigationEndpoint": {"watchEndpoint": {"videoId": VIDEO}},
        "playlistItemData": {"videoId": "zyxwvutsrqp"},
        "flexColumns": [{"musicResponsiveListItemFlexColumnRenderer": {"text": {"runs": [
            {"text": "2024", "navigationEndpoint": {"browseEndpoint": {"browseId": "UCabcdefghijklmnopqrstuv"}}},
            {"text": "Artist", "navigationEndpoint": {"browseEndpoint": {"browseId": "UCzyxwvutsrqponmlkjihgfe"}}}
        ]}}}]
    });
    assert!(select::parse_search_candidate(&item).is_none());
    item["playlistItemData"]["videoId"] = json!(VIDEO);
    let candidate = select::parse_search_candidate(&item).expect("matching video IDs");
    assert_eq!(candidate.channel_id, None);
    assert_eq!(candidate.duration_secs, None);
    for text in ["1:75", "+225", "1:60:00"] {
        assert_eq!(select::parse_duration(text), None, "{text}");
    }
}

#[test]
fn public_route_stays_closed_and_extract_rejects_url_extras() {
    assert_eq!(routes::decide_public_fetch().0, 404);
    let post = |extra| routes::decide_extract("POST", "/extract", extra).status();
    assert_eq!(post(None), 503);
    for extra in ["", "unexpected"] {
        assert_eq!(post(Some(extra)), 404);
    }
}
