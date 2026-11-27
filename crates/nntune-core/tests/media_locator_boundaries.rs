use nntune_core::{
    media::{HEADER_SIZE, PLAIN_CHUNK_SIZE, TAG_SIZE},
    media_envelope::{
        matches_scheme2, unwrap_scheme2, wrap_scheme2, EnvelopeContext, EnvelopeError,
    },
    media_layout::{plan_articles, plan_range, MAX_ARTICLE_SPANS},
    s3_locator::{S3Binding, S3Locator, S3LocatorError, S3MediaMetadata},
};

const BUCKET: &str = "fixture-media";
const KEY: &str = "media/00000000000040008000000000000001/00000000000040008000000000000002";
// Largest P for which 48 + P + 16 * ceil(P / 65536) fits in u64.
const MAX_PLAIN: u64 = 0xfff0_00ff_f000_ffbf;

fn context(plaintext_len: u64) -> EnvelopeContext {
    let mut id = [0; 16];
    id[6] = 0x40;
    id[8] = 0x80;
    let mut identities = [id; 6];
    identities[3][15] = 1;
    identities[4][15] = 2;
    EnvelopeContext {
        identities,
        plaintext_len,
        header: [7; 48],
    }
}

#[test]
fn locator_refuses_ambiguous_versions_but_preserves_opaque_bindings() {
    use S3LocatorError::Binding;

    for invalid in ["v1,v2", "v1, v2", "null", "", "v\u{7f}", "v\u{00e9}"] {
        let binding = S3Binding::VersionId(invalid.into());
        assert_eq!(
            S3Locator::new(BUCKET.into(), KEY.into(), binding).err(),
            Some(Binding)
        );
        let json =
            serde_json::json!({"bucket": BUCKET, "key": KEY, "binding": {"version_id": invalid}});
        assert_eq!(
            S3Locator::parse(json.to_string().as_bytes()).err(),
            Some(Binding)
        );
    }
    for binding in [
        S3Binding::VersionId("Fixture+/Version==".into()),
        S3Binding::VersionId("v".repeat(1024)),
        S3Binding::Etag("\"a,b\"".into()),
    ] {
        let locator = S3Locator::new(BUCKET.into(), KEY.into(), binding).unwrap();
        let json = serde_json::to_vec(&locator).unwrap();
        assert!(S3Locator::parse(&json).is_ok_and(|parsed| parsed == locator));
    }
    let over = S3Binding::VersionId("v".repeat(1025));
    assert_eq!(
        S3Locator::new(BUCKET.into(), KEY.into(), over).err(),
        Some(Binding)
    );
}

#[test]
fn locator_json_bounds_do_not_admit_authority_or_key_overrides() {
    use S3LocatorError::{Bucket, Key, Shape};

    let valid = serde_json::json!({"bucket": BUCKET, "key": KEY, "binding": {"version_id": "v1"}});
    for (field, raw, error) in [
        ("bucket", "https://fixture-media", Bucket),
        ("bucket", "fixture-media:443", Bucket),
        ("bucket", "[::1]", Bucket),
        ("bucket", "192.0.2.1", Bucket),
        ("key", "media/%2e%2e/fixture", Key),
    ] {
        let mut json = valid.clone();
        json[field] = raw.into();
        assert_eq!(
            S3Locator::parse(json.to_string().as_bytes()).err(),
            Some(error)
        );
    }
    let mut override_endpoint = valid.clone();
    override_endpoint["endpoint"] = "https://fixture.example".into();
    assert_eq!(
        S3Locator::parse(override_endpoint.to_string().as_bytes()).err(),
        Some(Shape)
    );
    let mut json = valid.to_string().into_bytes();
    json.resize(8192, b' ');
    assert!(S3Locator::parse(&json).is_ok());
    json.push(b' ');
    assert_eq!(S3Locator::parse(&json).err(), Some(Shape));
}

#[test]
fn envelope_layout_and_locator_agree_at_ciphertext_overflow() {
    let root = [1; 32];
    let key = [2; 32];
    let valid = context(MAX_PLAIN);
    let over = context(MAX_PLAIN + 1);
    let record = wrap_scheme2(&root, &valid, &key).unwrap();
    assert_eq!(&record[114..122], &u64::MAX.to_be_bytes());
    assert!(matches_scheme2(&record, &valid));
    assert_eq!(*unwrap_scheme2(&root, &valid, &record).unwrap(), key);

    let mut inconsistent = record;
    inconsistent[106..114].copy_from_slice(&over.plaintext_len.to_be_bytes());
    assert!(!matches_scheme2(&inconsistent, &over));
    assert_eq!(
        wrap_scheme2(&root, &over, &key),
        Err(EnvelopeError::InvalidContext)
    );

    let last = plan_range(MAX_PLAIN, MAX_PLAIN - 1, MAX_PLAIN - 1).unwrap();
    assert_eq!(last.ciphertext_end, u64::MAX);
    assert_eq!(
        last.ciphertext_end - last.ciphertext_start,
        MAX_PLAIN % PLAIN_CHUNK_SIZE + TAG_SIZE
    );
    assert!(plan_range(MAX_PLAIN + 1, 0, 0).is_none());
    let spans = plan_articles(MAX_PLAIN, u64::MAX).unwrap();
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].ciphertext_offset, HEADER_SIZE);
    assert_eq!(spans[0].ciphertext_len, u64::MAX - HEADER_SIZE);
    assert!(plan_articles(MAX_PLAIN + 1, u64::MAX).is_none());

    let locator =
        S3Locator::new(BUCKET.into(), KEY.into(), S3Binding::VersionId("v1".into())).unwrap();
    let metadata = S3MediaMetadata::from_trusted_context(locator.clone(), &valid).unwrap();
    assert_eq!(metadata.ciphertext_len(), u64::MAX);
    assert!(S3MediaMetadata::from_trusted_context(locator, &over).is_none());
}

#[test]
fn article_span_cap_allows_exact_boundary() {
    let size = MAX_ARTICLE_SPANS * PLAIN_CHUNK_SIZE;
    let spans = plan_articles(size, 1).unwrap();
    assert_eq!(spans.len() as u64, MAX_ARTICLE_SPANS);
    let last = spans.last().unwrap();
    assert_eq!(last.plaintext_end, size);
    assert_eq!(last.ciphertext_len, PLAIN_CHUNK_SIZE + TAG_SIZE);
    assert!(plan_articles(size + 1, 1).is_none());
}
