use std::cell::Cell;

use nntune_core::{
    media_envelope::EnvelopeContext,
    s3_locator::{S3Binding, S3Locator, S3LocatorError, S3MediaMetadata},
};
use nntune_stream::choose::{choose_profile_artifact, MediaRow, StreamProfile};

const BUCKET: &str = "fixture-media";
const KEY: &str = "media/00000000000040008000000000000001/00000000000040008000000000000002";
const VERSION: &str = "Returned+/Version==";
const ETAG: &str = "\"opaque-2\"";

fn locator(binding: S3Binding) -> S3Locator {
    S3Locator::new(BUCKET.into(), KEY.into(), binding).unwrap()
}

fn context() -> EnvelopeContext {
    let mut id = [0; 16];
    id[6] = 0x40;
    id[8] = 0x80;
    let mut identities = [id; 6];
    identities[3][15] = 1;
    identities[4][15] = 2;
    EnvelopeContext {
        identities,
        plaintext_len: 65_537,
        header: [7; 48],
    }
}

struct FakeTransport<'a> {
    objects: Vec<(S3Locator, &'a [u8])>,
    calls: Cell<usize>,
}

impl<'a> FakeTransport<'a> {
    fn get(&self, locator: &S3Locator) -> Option<&'a [u8]> {
        self.calls.set(self.calls.get() + 1);
        self.objects
            .iter()
            .find_map(|(expected, bytes)| (expected == locator).then_some(*bytes))
    }
}

#[test]
fn binding_round_trip_pins_exact_fake_object_without_affecting_choice() {
    let versioned = locator(S3Binding::VersionId(VERSION.into()));
    let nonversioned = locator(S3Binding::Etag(ETAG.into()));
    let transport = FakeTransport {
        objects: vec![
            (versioned.clone(), b"versioned"),
            (nonversioned.clone(), b"nonversioned"),
        ],
        calls: Cell::new(0),
    };
    let legacy = MediaRow {
        format: "opus",
        bitrate: 64,
        content_hash: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        encrypted_s3: None,
    };
    let chosen = choose_profile_artifact(&[legacy], StreamProfile::Opus64);
    for (locator, binding_json, body) in [
        (
            versioned,
            serde_json::json!({"version_id": VERSION}),
            &b"versioned"[..],
        ),
        (
            nonversioned,
            serde_json::json!({"etag": ETAG}),
            &b"nonversioned"[..],
        ),
    ] {
        let bytes = serde_json::to_vec(&locator).unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&bytes).unwrap(),
            serde_json::json!({"bucket": BUCKET, "key": KEY, "binding": binding_json}),
        );
        let parsed = S3Locator::parse(&bytes).unwrap();
        assert_eq!(parsed.bucket(), BUCKET);
        assert_eq!(parsed.key(), KEY);
        assert!(parsed.binding() == locator.binding());
        let metadata = S3MediaMetadata::from_trusted_context(parsed, &context()).unwrap();
        let row = MediaRow {
            encrypted_s3: Some(&metadata),
            ..legacy
        };
        let before = transport.calls.get();
        assert_eq!(
            choose_profile_artifact(&[row], StreamProfile::Opus64),
            chosen
        );
        assert_eq!(transport.calls.get(), before);
        assert_eq!(transport.get(metadata.locator()), Some(body));
    }
    for wrong in [
        locator(S3Binding::VersionId("different-version".into())),
        locator(S3Binding::Etag("\"different-tag\"".into())),
        locator(S3Binding::VersionId(ETAG.into())),
    ] {
        assert_eq!(transport.get(&wrong), None);
    }
    assert_eq!(transport.calls.get(), 5);
}

#[test]
fn opaque_etag_cannot_establish_lengths_context_or_body_identity() {
    let etag = locator(S3Binding::Etag(ETAG.into()));
    let mut trusted = context();
    let metadata = S3MediaMetadata::from_trusted_context(etag.clone(), &trusted).unwrap();
    assert_eq!(metadata.plaintext_len(), 65_537);
    assert_eq!(metadata.ciphertext_len(), 65_617);
    assert!(metadata.matches_context(&trusted));
    // An envelope rewrap may change only the root version, not this context.
    trusted.identities[0][15] ^= 1;
    assert!(metadata.matches_context(&trusted));
    trusted.header[0] ^= 1;
    assert!(!metadata.matches_context(&trusted));
    let mut trusted = context();
    trusted.plaintext_len += 1;
    assert!(!metadata.matches_context(&trusted));
    let mut trusted = context();
    trusted.identities[4][15] ^= 1;
    assert!(!metadata.matches_context(&trusted));
    assert!(S3MediaMetadata::from_trusted_context(etag.clone(), &trusted).is_none());

    // A replacement can claim the same tag. Body integrity still depends on
    // independent trusted expectations, not successful locator selection.
    let fake = FakeTransport {
        objects: vec![(etag, b"changed")],
        calls: Cell::new(0),
    };
    assert_eq!(fake.get(metadata.locator()), Some(&b"changed"[..]));
    assert_ne!(fake.get(metadata.locator()), Some(&b"trusted"[..]));
}

#[test]
fn rejects_invalid_and_ambiguous_locator_json() {
    use S3LocatorError::{Binding, Bucket, Key, Shape};

    let rejected = |json: &str, error| {
        assert_eq!(
            S3Locator::parse(json.as_bytes()).err(),
            Some(error),
            "{json}"
        );
    };
    let prefix = format!(r#"{{"bucket":"{BUCKET}","key":"{KEY}""#);
    for suffix in [
        "}",
        r#", "binding":null}"#,
        r#", "binding":{}}"#,
        r#", "binding":{"version_id":"v","etag":"\"e\""}}"#,
        r#", "binding":{"etag":"\"e\"","version_id":"v"}}"#,
        r#", "binding":{"version_id":"v","version_id":"other"}}"#,
        r#", "binding":{"version_id":"v"}, "binding":{"version_id":"v"}}"#,
        r#", "binding":{"version_id":"v"}, "endpoint":"override"}"#,
        r#", "binding":{"version_id":"v"}} {}"#,
    ] {
        rejected(&format!("{prefix}{suffix}"), Shape);
    }
    let array = serde_json::json!([BUCKET, KEY, {"version_id": VERSION}]);
    rejected(&array.to_string(), Shape);
    let mut oversized = serde_json::to_vec(&locator(S3Binding::VersionId(VERSION.into()))).unwrap();
    oversized.resize(8192, b' ');
    assert!(S3Locator::parse(&oversized).is_ok());
    oversized.push(b' ');
    assert_eq!(S3Locator::parse(&oversized).err(), Some(Shape));

    let valid =
        serde_json::json!({"bucket": BUCKET, "key": KEY, "binding": {"version_id": VERSION}});
    for (field, invalid, error) in [
        ("bucket", "TwoWords", Bucket),
        ("bucket", "bad/bucket", Bucket),
        ("bucket", "bad:443", Bucket),
        ("bucket", "bad..bucket", Bucket),
        ("bucket", "192.0.2.7", Bucket),
        ("key", "", Key),
        ("key", "../escape", Key),
        ("key", "media/%2e%2e/b", Key),
        ("key", "media/a/b?versionId=x", Key),
    ] {
        let mut json = valid.clone();
        json[field] = serde_json::json!(invalid);
        rejected(&json.to_string(), error);
    }
    for (kind, value) in [
        ("version_id", "null"),
        ("version_id", ""),
        ("version_id", "v\n2"),
        ("version_id", " v2"),
        ("etag", "*"),
        ("etag", "unquoted"),
        ("etag", "W/\"weak\""),
        ("etag", "\"a\",\"b\""),
        ("etag", "\"\""),
    ] {
        let json = serde_json::json!({"bucket": BUCKET, "key": KEY, "binding": { (kind): value }});
        rejected(&json.to_string(), Binding);
    }
}
