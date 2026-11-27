use nntune_subsonic::construction_budget::{BudgetExceeded, ConstructionBudget};
use nntune_subsonic::envelope::{code, error_document, API_VERSION, JSON_CONTENT_TYPE};
use nntune_subsonic::staging_browse::MemberBrowse;
use nntune_subsonic::staging_endpoints::basic_document;
use nntune_subsonic::staging_rejection::{
    fixed_staging_rejection, RejectionFormat, StagingRejection,
};
use nntune_subsonic::value::Value;
use nntune_subsonic::{Library, MemStore, Params};

const EMPTY: MemberBrowse<'static> = MemberBrowse {
    artists: &[],
    albums: &[],
    songs: &[],
    now_millis: 0,
};

fn browse(
    view: &MemberBrowse<'_>,
    budget: &mut ConstructionBudget,
    endpoint: &str,
    query: &str,
) -> Result<Option<Value>, BudgetExceeded> {
    view.document(budget, endpoint, &Params::parse(query), |_| {
        panic!("unexpected direct song lookup")
    })
}

fn field<'a>(value: &'a Value, key: &str) -> &'a Value {
    let Value::Map(fields) = value else {
        panic!("missing map for {key}");
    };
    &fields.iter().find(|(name, _)| name == key).unwrap().1
}

fn assert_missing(view: &MemberBrowse<'_>, endpoint: &str, query: &str) {
    let actual = browse(view, &mut ConstructionBudget::new(4096), endpoint, query);
    let expected = error_document(
        code::REQUIRED_PARAM_MISSING,
        "Required parameter is missing.",
    );
    assert_eq!(actual, Ok(Some(expected)), "{endpoint} {query}");
}

#[test]
fn search_rejects_invalid_windows_and_oversized_decoded_queries() {
    let songs = MemStore::seeded().all_songs();
    let view = MemberBrowse {
        songs: &songs,
        ..EMPTY
    };
    for query in [
        "size=501",
        "count=501",
        "size=invalid&offset=0",
        "count=-1",
        "offset=",
        "offset=-1",
        "offset=1025",
        "offset=18446744073709551616",
        "songCount=501",
    ] {
        assert_missing(&view, "search3", query);
    }
    let at_limit = format!("query={}", "%C3%A9".repeat(512));
    for endpoint in ["search2", "search3"] {
        let mut budget = ConstructionBudget::new(4096);
        let doc = browse(&view, &mut budget, endpoint, &at_limit)
            .unwrap()
            .unwrap();
        assert_eq!(field(&doc, "status"), &Value::Str("ok".into()));
        assert_missing(&view, endpoint, &format!("{at_limit}x"));
    }
}

#[test]
fn legacy_search_refuses_overflow_without_a_partial_page() {
    let songs = vec![MemStore::seeded().all_songs().remove(0); 501];
    let mut budget = ConstructionBudget::new(usize::MAX);
    for (endpoint, query, result, songs) in [
        ("search2", "query=Test", "searchResult2", &songs[..500]),
        ("search3", "size=500", "searchResult3", &songs[..]),
    ] {
        let view = MemberBrowse { songs, ..EMPTY };
        let doc = browse(&view, &mut budget, endpoint, query)
            .unwrap()
            .unwrap();
        let Value::List(matches) = field(field(&doc, result), "song") else {
            panic!("missing songs");
        };
        assert_eq!(matches.len(), 500);
    }
    let view = MemberBrowse {
        songs: &songs,
        ..EMPTY
    };
    let overflow = browse(&view, &mut budget, "search2", "query=Test");
    let failure = error_document(code::GENERIC, "Too many search results. Narrow the query.");
    assert_eq!(overflow, Ok(Some(failure)));
}

#[test]
fn album_pages_validate_before_sorting_and_skip_past_end_work() {
    let albums = MemStore::seeded().all_albums();
    let view = MemberBrowse {
        albums: &albums,
        ..EMPTY
    };
    for endpoint in ["getAlbumList", "getAlbumList2"] {
        for query in [
            "type=newest&size=501",
            "type=newest&offset=1025",
            "type=byYear&fromYear=-32769&toYear=2026",
            "type=byYear&fromYear=2020&toYear=32768",
        ] {
            assert_missing(&view, endpoint, query);
        }
        let query = "type=newest&size=500&offset=1024";
        let mut probe = ConstructionBudget::new(usize::MAX);
        let empty_page = browse(&EMPTY, &mut probe, endpoint, query).unwrap();
        assert_eq!(
            field(empty_page.as_ref().unwrap(), "status"),
            &Value::Str("ok".into())
        );
        let mut exact = ConstructionBudget::new(probe.charged());
        assert_eq!(browse(&view, &mut exact, endpoint, query), Ok(empty_page));
    }
}

#[test]
fn direct_lookup_refuses_bad_ids_and_ambiguous_occurrences() {
    let song = MemStore::seeded().all_songs().remove(0);
    let songs = [song.clone(), song];
    let view = MemberBrowse {
        songs: &songs,
        ..EMPTY
    };
    for query in ["id=so-1%00", &format!("id={}", "é".repeat(513))] {
        assert_missing(&view, "getSong", query);
    }
    let mut budget = ConstructionBudget::new(4096);
    let ambiguous = browse(&view, &mut budget, "getSong", "id=so-1");
    assert_eq!(
        ambiguous,
        Ok(Some(error_document(code::NOT_FOUND, "Song not found.")))
    );
}

#[test]
fn license_never_reports_valid_for_an_unusable_account_email() {
    let mut budget = ConstructionBudget::new(usize::MAX);
    let failed = error_document(code::GENERIC, "License unavailable.");
    let at_limit = format!("{}x@example.test", "é".repeat(505));
    let over_limit = at_limit.clone() + "x";
    for email in [
        "",
        " ",
        "a\u{0001}@example.test",
        "a\u{ffff}@example.test",
        &over_limit,
    ] {
        assert_eq!(
            basic_document(&mut budget, "getLicense", email),
            Ok(Some(failed.clone()))
        );
    }
    let doc = basic_document(&mut budget, "getLicense", &at_limit)
        .unwrap()
        .unwrap();
    assert_eq!(
        field(field(&doc, "license"), "email"),
        &Value::Str(at_limit)
    );
}

#[test]
fn fixed_rejections_keep_status_retry_and_wire_format_consistent() {
    use StagingRejection::{BusyOrCapacity, RequestOrDocumentTooLarge, Unavailable};
    let format = RejectionFormat::from_safe_selector(Some("jsonp"));
    for (reason, status, retry) in [
        (RequestOrDocumentTooLarge, 413, None),
        (BusyOrCapacity, 503, Some("1")),
        (Unavailable, 503, None),
    ] {
        let response = fixed_staging_rejection(reason, format);
        assert_eq!((response.status, response.retry_after), (status, retry));
        assert_eq!(response.content_type, JSON_CONTENT_TYPE);
        let text = std::str::from_utf8(response.body).unwrap();
        assert!(text.starts_with(r#"{"subsonic-response":{"status":"failed""#));
        assert!(text.contains(&format!(r#""version":"{API_VERSION}""#)));
        assert!(text.contains(&format!(r#""error":{{"code":{},"message":"#, code::GENERIC)));
    }
}
