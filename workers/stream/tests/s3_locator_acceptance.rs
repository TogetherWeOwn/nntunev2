use std::cell::Cell;

use nntune_core::{
    digest_geometry::DigestGeometry,
    digest_readback::ReadbackBody,
    media_envelope::EnvelopeContext,
    s3_locator::{S3Binding, S3Locator, S3LocatorError, S3MediaMetadata},
};
use nntune_stream::{
    choose::{choose_profile_artifact, MediaRow, StreamProfile},
    s3_range_response::{read_s3_range_chunk, S3RangeResponse, S3RangeResponseError},
};

const BUCKET: &str = "fixture-media";
const OBJ_ID: &str = "00000000000040008000000000000001";
const GEN_ID: &str = "00000000000040008000000000000002";
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

#[test]
fn matches_target_and_trusted_context_enforce_canonical_uuid_and_bounds() {
    let trusted = context();
    let max_bucket = "a".repeat(63);
    for bucket in [BUCKET, "abc", "media-test.v1", "1.2.3", max_bucket.as_str()] {
        assert!(
            S3MediaMetadata::matches_target(bucket, KEY, &trusted),
            "{bucket}"
        );
    }

    let over_bucket = "a".repeat(64);
    for bucket in [
        "ab",
        over_bucket.as_str(),
        ".abc",
        "abc.",
        "-abc",
        "abc-",
        "a.-b",
        "a-.b",
        "bad/bucket",
        "bad\\bucket",
        "../bucket",
        "192.0.2.7",
        "Bad-bucket",
    ] {
        assert!(
            !S3MediaMetadata::matches_target(bucket, KEY, &trusted),
            "{bucket}"
        );
    }

    let swapped_key = format!("media/{GEN_ID}/{OBJ_ID}");
    let other_object = KEY.replacen("0001/", "0003/", 1);
    let other_gen = KEY.replacen("0002", "0004", 1);
    for bad_key in [
        format!("/{KEY}"),
        format!("../{KEY}"),
        format!("{KEY}/.."),
        format!("{KEY}/../extra"),
        format!("{KEY}/extra"),
        format!("media/{OBJ_ID}/../{GEN_ID}"),
        format!("media/%2e%2e/{GEN_ID}"),
        KEY.replace('/', "\\"),
        KEY.to_uppercase(),
        swapped_key,
        other_object,
        other_gen,
    ] {
        assert!(
            !S3MediaMetadata::matches_target(BUCKET, &bad_key, &trusted),
            "{bad_key}"
        );
    }

    let mut hex_ctx = context();
    hex_ctx.identities[3][8] = 0xa0;
    hex_ctx.identities[4][8] = 0xb0;
    let hex_obj = "0000000000004000a000000000000001";
    let hex_gen = "0000000000004000b000000000000002";
    let hex_key = format!("media/{hex_obj}/{hex_gen}");
    assert!(S3MediaMetadata::matches_target(BUCKET, &hex_key, &hex_ctx));
    for upper_uuid_key in [
        format!("media/{}/{hex_gen}", hex_obj.to_uppercase()),
        format!("media/{hex_obj}/{}", hex_gen.to_uppercase()),
    ] {
        assert!(
            !S3MediaMetadata::matches_target(BUCKET, &upper_uuid_key, &hex_ctx),
            "{upper_uuid_key}"
        );
    }

    let valid_locator = locator(S3Binding::VersionId(VERSION.into()));
    let metadata = S3MediaMetadata::from_trusted_context(valid_locator.clone(), &trusted).unwrap();
    let etag_metadata =
        S3MediaMetadata::from_trusted_context(locator(S3Binding::Etag(ETAG.into())), &trusted)
            .unwrap();
    for entry in [&metadata, &etag_metadata] {
        let redacted = format!("{entry:?}");
        assert!(redacted.contains("S3MediaMetadata"));
        for secret in [BUCKET, KEY, VERSION, ETAG] {
            assert!(!redacted.contains(secret), "{redacted}");
        }
    }

    let overflow_len = EnvelopeContext {
        plaintext_len: u64::MAX,
        ..trusted
    };
    assert!(S3MediaMetadata::from_trusted_context(valid_locator.clone(), &overflow_len).is_none());

    let mut rewrapped = context();
    rewrapped.identities[0][15] ^= 1;
    assert!(metadata.matches_context(&rewrapped));

    for slot in 1..6 {
        let mut shifted = context();
        shifted.identities[slot][15] ^= 1;
        assert!(!metadata.matches_context(&shifted), "slot {slot}");

        let mut bad_version = context();
        bad_version.identities[slot][6] = 0x50;
        assert!(
            !S3MediaMetadata::matches_target(BUCKET, KEY, &bad_version),
            "version slot {slot}"
        );
        assert!(
            S3MediaMetadata::from_trusted_context(valid_locator.clone(), &bad_version).is_none(),
            "version slot {slot}"
        );

        for variant in [0x40, 0xc0] {
            let mut bad_variant = context();
            bad_variant.identities[slot][8] = variant;
            assert!(
                !S3MediaMetadata::matches_target(BUCKET, KEY, &bad_variant),
                "variant {variant:#x} slot {slot}"
            );
            assert!(
                S3MediaMetadata::from_trusted_context(valid_locator.clone(), &bad_variant)
                    .is_none(),
                "variant {variant:#x} slot {slot}"
            );
        }
    }
}

#[tokio::test]
async fn parse_rejects_traversal_uuid_and_binding_bounds_and_isolates_cross_bucket() {
    use S3LocatorError::{Binding, Bucket, Key};

    struct CountingBody<'a>(&'a [u8], &'a Cell<usize>);
    impl ReadbackBody for CountingBody<'_> {
        async fn read(&mut self, dst: &mut [u8]) -> std::io::Result<usize> {
            self.1.set(self.1.get() + 1);
            std::io::Read::read(&mut self.0, dst)
        }
    }

    let max_bucket = "a".repeat(63);
    let max_version = "v".repeat(1024);
    let max_etag = format!("\"{}\"", "e".repeat(1024));
    let hex_key = "media/0000000000004000a000000000000001/0000000000004000b000000000000002";
    for candidate in [
        S3Locator::new("abc".into(), KEY.into(), S3Binding::VersionId(max_version)).unwrap(),
        S3Locator::new(max_bucket, KEY.into(), S3Binding::Etag(max_etag)).unwrap(),
        S3Locator::new(
            "1.2.3".into(),
            hex_key.into(),
            S3Binding::VersionId(VERSION.into()),
        )
        .unwrap(),
    ] {
        let wire = serde_json::to_vec(&candidate).unwrap();
        let parsed = S3Locator::parse(&wire).unwrap();
        assert!(parsed == candidate);
    }

    let valid =
        serde_json::json!({"bucket": BUCKET, "key": KEY, "binding": {"version_id": VERSION}});
    let over_bucket = "a".repeat(64);
    for bad_bucket in [
        "ab",
        over_bucket.as_str(),
        ".abc",
        "abc.",
        "-abc",
        "abc-",
        "a.-b",
        "a-.b",
        "../bucket",
        "bad\\bucket",
    ] {
        let mut json = valid.clone();
        json["bucket"] = serde_json::json!(bad_bucket);
        assert_eq!(
            S3Locator::parse(json.to_string().as_bytes()).err(),
            Some(Bucket),
            "{bad_bucket}"
        );
    }

    let bad_obj_ver = format!("media/00000000000050008000000000000001/{GEN_ID}");
    let bad_gen_ver = format!("media/{OBJ_ID}/00000000000050008000000000000002");
    let bad_obj_var_low = format!("media/00000000000040004000000000000001/{GEN_ID}");
    let bad_gen_var_high = format!("media/{OBJ_ID}/0000000000004000c000000000000002");
    let upper_obj_hex = format!("media/0000000000004000A000000000000001/{GEN_ID}");
    let upper_gen_hex = format!("media/{OBJ_ID}/0000000000004000B000000000000002");
    for bad_key in [
        bad_obj_ver,
        bad_gen_ver,
        bad_obj_var_low,
        bad_gen_var_high,
        upper_obj_hex,
        upper_gen_hex,
        KEY.to_uppercase(),
        format!("/{KEY}"),
        format!("../{KEY}"),
        format!("{KEY}/.."),
        format!("{KEY}/../other"),
        format!("media/{OBJ_ID}/../{GEN_ID}"),
        format!("{KEY}/extra"),
        KEY.replace('/', "\\"),
        format!("{KEY}\0"),
    ] {
        let mut json = valid.clone();
        json["key"] = serde_json::json!(bad_key);
        assert_eq!(
            S3Locator::parse(json.to_string().as_bytes()).err(),
            Some(Key),
            "{bad_key}"
        );
    }

    let over_version = "v".repeat(1025);
    let over_etag = format!("\"{}\"", "e".repeat(1025));
    for (kind, value) in [("version_id", over_version), ("etag", over_etag)] {
        let json = serde_json::json!({"bucket": BUCKET, "key": KEY, "binding": { (kind): value }});
        assert_eq!(
            S3Locator::parse(json.to_string().as_bytes()).err(),
            Some(Binding),
            "{kind}"
        );
    }

    // Verify cross-bucket and cross-object locators fail closed at both
    // S3MediaMetadata::from_trusted_context and production read_s3_range_chunk
    // with zero body reads while keeping stored metadata intact.
    let trusted = context();
    let versioned = locator(S3Binding::VersionId(VERSION.into()));
    let stored = S3MediaMetadata::from_trusted_context(versioned.clone(), &trusted).unwrap();
    let cross_bucket_json = serde_json::json!({
        "bucket": "other-bucket",
        "key": KEY,
        "binding": {"version_id": VERSION}
    })
    .to_string();
    let cross_bucket = S3Locator::parse(cross_bucket_json.as_bytes()).unwrap();
    assert!(&cross_bucket != stored.locator());

    let cross_object_json = serde_json::json!({
        "bucket": BUCKET,
        "key": KEY.replacen("0001/", "0003/", 1),
        "binding": {"version_id": VERSION}
    })
    .to_string();
    let cross_object = S3Locator::parse(cross_object_json.as_bytes()).unwrap();
    assert!(S3MediaMetadata::from_trusted_context(cross_object.clone(), &trusted).is_none());

    let plan = DigestGeometry::new(stored.plaintext_len())
        .unwrap()
        .range(65_536, 65_537)
        .unwrap();
    let response = S3RangeResponse {
        status: 206,
        content_range: Some("bytes 65600-65616/65617"),
        content_length: Some(17),
        binding: Some(stored.locator().binding()),
    };
    let payload = [0x5a_u8; 17];
    let reads = Cell::new(0);
    for rejected_locator in [&cross_bucket, &cross_object] {
        reads.set(0);
        assert_eq!(
            read_s3_range_chunk(
                plan,
                1,
                stored.locator(),
                rejected_locator,
                response,
                CountingBody(&payload, &reads),
                || true,
            )
            .await,
            Err(S3RangeResponseError::ObjectIdentity)
        );
        assert_eq!(reads.get(), 0);
    }

    assert_eq!(stored.locator().bucket(), BUCKET);
    assert_eq!(stored.locator().key(), KEY);
    assert!(stored.locator().binding() == &S3Binding::VersionId(VERSION.into()));
    assert_eq!(stored.plaintext_len(), 65_537);
    assert_eq!(stored.ciphertext_len(), 65_617);
    assert!(stored.matches_context(&trusted));
    assert_eq!(
        read_s3_range_chunk(
            plan,
            1,
            stored.locator(),
            stored.locator(),
            response,
            CountingBody(&payload, &reads),
            || true,
        )
        .await,
        Ok(payload.to_vec())
    );
}
