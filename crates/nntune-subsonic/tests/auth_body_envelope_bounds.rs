use nntune_subsonic::auth::authenticate;
use nntune_subsonic::envelope::{
    code, error_document, ok_document, render_json, render_xml, JSONP_CONTENT_TYPE,
    JSON_CONTENT_TYPE, XML_CONTENT_TYPE,
};
use nntune_subsonic::staging_rejection::{
    fixed_staging_rejection, RejectionFormat, StagingRejection,
};
use nntune_subsonic::value::Value;
use nntune_subsonic::{
    AuthStore, BodyFault, Credentials, FormBody, Identity, MemStore, SubsonicApp,
};

struct NoLookup;

impl AuthStore for NoLookup {
    fn verify_api_key(&self, _: &str) -> Option<Identity> {
        panic!("refusal reached the API key store")
    }

    fn verify_password(&self, _: &str, _: &str) -> Option<Identity> {
        panic!("refusal reached the password store")
    }

    fn verify_token(&self, _: &str, _: &str, _: &str) -> Option<Identity> {
        panic!("refusal reached the token store")
    }
}

#[test]
fn empty_api_key_and_mixed_auth_are_refused_before_lookup() {
    let empty_key = Credentials {
        api_key: Some(String::new()),
        ..Credentials::default()
    };
    assert_eq!(
        authenticate(&NoLookup, &empty_key).unwrap_err().0,
        code::INVALID_API_KEY
    );

    for field in ["u", "p", "t", "s"] {
        let creds = Credentials::from_params(|key| match key {
            "apiKey" => Some("fixture-key".into()),
            key if key == field => Some(String::new()),
            _ => None,
        });
        assert_eq!(
            authenticate(&NoLookup, &creds).unwrap_err().0,
            code::CONFLICTING_AUTH
        );
    }

    let app = SubsonicApp::new(MemStore::empty_with_test_auth());
    let response = app.handle("GET", "/rest/ping", "apiKey=&u=test&p=test-pass&f=json", "");
    assert_eq!(response.status, 200);
    assert_eq!(response.content_type, JSON_CONTENT_TYPE);
    assert_eq!(
        response.body,
        render_json(&error_document(
            code::CONFLICTING_AUTH,
            "Conflicting authentication parameters.",
        ))
    );
}

#[test]
fn malformed_selected_credentials_do_not_fall_back_or_reach_storage() {
    for invalid in ["enc:", "enc:1", "enc:zz", "enc:ff"] {
        let creds = Credentials {
            username: Some("test".into()),
            password: Some(invalid.into()),
            ..Credentials::default()
        };
        assert_eq!(
            authenticate(&NoLookup, &creds).unwrap_err().0,
            code::WRONG_CREDENTIALS
        );
    }

    let mut creds = Credentials {
        username: Some("test".into()),
        password: Some("test-pass".into()),
        token: Some("fixture-invalid-token".into()),
        ..Credentials::default()
    };
    assert_eq!(
        authenticate(&NoLookup, &creds).unwrap_err().0,
        code::REQUIRED_PARAM_MISSING
    );
    creds.salt = Some("fixture-salt".into());
    let failure = authenticate(&MemStore::empty_with_test_auth(), &creds).unwrap_err();
    assert_eq!(
        failure,
        (
            code::WRONG_CREDENTIALS,
            "Wrong username or password.".into()
        )
    );
}

#[test]
fn credential_diagnostics_exclude_values_and_raw_form_bytes() {
    let fields = [
        ("apiKey", "synthetic-key"),
        ("u", "synthetic-user"),
        ("p", "synthetic-password"),
        ("t", "synthetic-token"),
        ("s", "synthetic-salt"),
    ];
    let creds = Credentials::from_params(|key| {
        fields
            .iter()
            .find_map(|&(name, value)| (name == key).then(|| value.to_owned()))
    });
    let debug = format!("{creds:#?}");
    let raw = b"p=synthetic-password";
    let mut body = FormBody::with_cap(None, 64);
    body.push(raw).unwrap();
    for (_, secret) in fields {
        assert!(!debug.contains(secret));
    }
    // Byte vectors use numbers in Debug, so searching for the string alone
    // would miss the original disclosure.
    assert!(format!("{body:?}") == "FormBody { .. }");
}

#[test]
fn form_oversize_is_permanent_and_declared_oversize_never_accepts_bytes() {
    let mut declared = FormBody::with_cap(Some(11), 10);
    assert_eq!(declared.check_declared(), Err(BodyFault::Oversized));
    assert_eq!(declared.push(b"u=test"), Err(BodyFault::Oversized));
    assert_eq!(declared.finish(), Err(BodyFault::Oversized));

    let mut streamed = FormBody::with_cap(Some(6), 10);
    streamed.push(b"u=test").unwrap();
    assert_eq!(streamed.push(b"&p=123"), Err(BodyFault::Oversized));
    assert_eq!(streamed.push(b""), Err(BodyFault::Oversized));
    assert_eq!(streamed.finish(), Err(BodyFault::Oversized));
}

#[test]
fn form_exact_byte_cap_retains_split_utf8_but_short_or_invalid_input_fails() {
    let raw = "apiKey=test-key-%31&c=Café".as_bytes();
    let (prefix, last_byte) = raw.split_at(raw.len() - 1);
    let mut exact = FormBody::with_cap(Some(raw.len()), raw.len());
    exact.push(prefix).unwrap();
    exact.push(last_byte).unwrap();
    let body = exact.finish().unwrap();
    assert_eq!(body.as_bytes(), raw);

    let app = SubsonicApp::new(MemStore::empty_with_test_auth());
    let response = app.handle("POST", "/rest/ping", "f=json", &body);
    assert_eq!(response.body, render_json(&ok_document(vec![])));

    let mut short = FormBody::with_cap(Some(raw.len()), raw.len());
    short.push(b"u=test").unwrap();
    assert_eq!(short.finish(), Err(BodyFault::Unreadable));

    let mut invalid = FormBody::with_cap(None, raw.len());
    invalid.push(prefix).unwrap();
    assert_eq!(invalid.finish(), Err(BodyFault::Unreadable));
}

#[test]
fn xml_and_json_escape_envelope_data_for_their_own_context() {
    let message = "Café <&>\"' ]]>";
    let xml = String::from_utf8(render_xml(&error_document(10, message))).unwrap();
    assert!(xml.ends_with(
        "<error code=\"10\" message=\"Café &lt;&amp;&gt;&quot;' ]]&gt;\"/></subsonic-response>"
    ));
    let doc = ok_document(vec![(
        "note".into(),
        Value::map(vec![("value", Value::str_value(message))]),
    )]);
    let xml = String::from_utf8(render_xml(&doc)).unwrap();
    assert!(xml.ends_with("<note>Café &lt;&amp;&gt;&quot;' ]]&gt;</note></subsonic-response>"));

    let json =
        String::from_utf8(render_json(&error_document(10, "Café \"\\\r\n\t\0\u{1f}"))).unwrap();
    assert!(json.ends_with(r#""error":{"code":10,"message":"Café \"\\\r\n\t\u0000\u001f"}}}"#));
}

#[test]
fn raw_line_breaks_in_jsonp_callbacks_return_plain_json() {
    for app in [
        SubsonicApp::new(MemStore::empty_with_test_auth()),
        SubsonicApp::production(MemStore::empty_with_test_auth()),
    ] {
        for line_break in ["%0A", "%0D", "%0D%0A"] {
            let query = format!("apiKey=test-key-1&f=jsonp&callback=cb['first{line_break}second']");
            let response = app.handle("GET", "/rest/ping", &query, "");
            assert_eq!(response.content_type, JSON_CONTENT_TYPE);
            assert_eq!(
                response.body,
                render_json(&error_document(10, "Required parameter is missing."))
            );
        }
        let valid = app.handle(
            "GET",
            "/rest/ping",
            "apiKey=test-key-1&f=jsonp&callback=cb['a%5Cnb']",
            "",
        );
        assert_eq!(valid.content_type, JSONP_CONTENT_TYPE);
        assert!(valid.body.starts_with(br"/**/cb['a\nb']({"));
    }
}

#[test]
fn staging_rejections_are_bounded_static_documents_with_fixed_headers() {
    for (reason, status, retry, message) in [
        (
            StagingRejection::RequestOrDocumentTooLarge,
            413,
            None,
            "Request or document too large.",
        ),
        (
            StagingRejection::BusyOrCapacity,
            503,
            Some("1"),
            "Service unavailable.",
        ),
        (
            StagingRejection::Unavailable,
            503,
            None,
            "Service unavailable.",
        ),
    ] {
        let json = format!(
            r#"{{"subsonic-response":{{"status":"failed","version":"1.16.1","error":{{"code":0,"message":"{message}"}}}}}}"#
        );
        let xml = format!(
            r#"<subsonic-response status="failed" version="1.16.1"><error code="0" message="{message}"/></subsonic-response>"#
        );
        for (selector, content_type, expected) in [
            (Some("json"), JSON_CONTENT_TYPE, &json),
            (Some("jsonp"), JSON_CONTENT_TYPE, &json),
            (None, XML_CONTENT_TYPE, &xml),
            (
                Some("jsonp&callback=synthetic-marker"),
                XML_CONTENT_TYPE,
                &xml,
            ),
        ] {
            let response =
                fixed_staging_rejection(reason, RejectionFormat::from_safe_selector(selector));
            assert_eq!(response.status, status);
            assert_eq!(response.retry_after, retry);
            assert_eq!(response.content_type, content_type);
            assert!(response.body.len() <= 1024);
            assert_eq!(response.body, expected.as_bytes());
        }
    }
}
