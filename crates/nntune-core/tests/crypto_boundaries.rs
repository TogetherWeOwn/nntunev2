use nntune_core::{
    media::{
        KeyMode, Profile, VerifiedHeader, CIPHER_CHUNK_SIZE, HEADER_SIZE, PLAIN_CHUNK_SIZE,
        TAG_SIZE,
    },
    media_envelope::{wrap_scheme2, EnvelopeContext},
    media_layout::{plan_articles, plan_range},
    origin_sealer::OriginSealer,
    playback_custody::{signature, CustodySnapshot, LEASE_PATH, RESOLVE_PATH},
    s3_signing::{encode_query_value, S3SigningKey},
};

const DATE: &str = "20261124T074000Z";
const HASH: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
const NAMES: &str = "host;range;x-amz-content-sha256;x-amz-date";

fn signing_fixture(date: &str) -> (S3SigningKey, String) {
    let key = S3SigningKey::new(
        "fixture-region".into(),
        "FIXTUREACCESS".into(),
        "fixture-secret".into(),
    )
    .unwrap();
    let canonical = format!(
        "GET\n/bucket/media%20file\na=A%2BB&versionId=%2F\nhost:fixtures.invalid\nrange:bytes=0-9\nx-amz-content-sha256:{HASH}\nx-amz-date:{date}\n\n{NAMES}\n{HASH}"
    );
    (key, canonical)
}

#[test]
fn canonical_s3_signature_matches_independent_vector_without_debug_leakage() {
    let (key, canonical) = signing_fixture(DATE);
    let authorization = key.authorization(DATE, &canonical, NAMES).unwrap();
    let expected = "00d7c22571bc2fa006906f40756a79569c9bade5d3a1efe8f24c41a445b9703c";
    assert!(authorization.as_str() == format!(
        "AWS4-HMAC-SHA256 Credential=FIXTUREACCESS/20261124/fixture-region/s3/aws4_request, SignedHeaders={NAMES}, Signature={expected}"
    ));
    let rendered = format!("{authorization:?}");
    for sensitive in ["FIXTUREACCESS", "fixture-secret", expected] {
        assert!(!rendered.contains(sensitive));
    }
    assert_eq!(
        encode_query_value("a /+&=%é~"),
        "a%20%2F%2B%26%3D%25%C3%A9~"
    );
}

#[test]
fn s3_signer_refuses_noncanonical_targets_and_headers() {
    let (key, canonical) = signing_fixture(DATE);
    let cases = [
        (
            "encoded query order",
            "a=A%2BB&versionId=%2F",
            "versionId=%2F&a=A%2BB",
        ),
        ("noncanonical escape", "media%20file", "media%20%66ile"),
        ("URL-normalized path", "/bucket/media", "/bucket/../media"),
        ("raw query delimiter", "a=A%2BB", "a=A+B"),
        (
            "presigned expiry",
            "a=A%2BB&versionId=%2F",
            "X-Amz-Expires=60&a=A%2BB&versionId=%2F",
        ),
        (
            "header carriage return",
            "fixtures.invalid",
            "fixtures.invalid\r",
        ),
        ("header whitespace", "bytes=0-9", " bytes=0-9"),
        (
            "header duplication",
            "range:bytes=0-9",
            "range:bytes=0-9\nrange:bytes=9-9",
        ),
        ("date mismatch", DATE, "20261124T074001Z"),
    ];
    let accepted: Vec<_> = cases
        .into_iter()
        .filter_map(|(case, from, to)| {
            key.authorization(DATE, &canonical.replace(from, to), NAMES)
                .map(|_| case)
        })
        .collect();
    assert!(accepted.is_empty(), "signed malformed input: {accepted:?}");
    for names in ["host", "host;host", "host\r\nX-Test: bad"] {
        assert!(key.authorization(DATE, &canonical, names).is_none());
    }
}

#[test]
fn s3_signer_requires_a_real_utc_date_and_bound_payload_hash() {
    for date in [
        "20260229T074000Z",
        "19000229T074000Z",
        "20261124T240000Z",
        "20261124T076000Z",
    ] {
        let (key, canonical) = signing_fixture(date);
        assert!(
            key.authorization(date, &canonical, NAMES).is_none(),
            "accepted {date}"
        );
    }
    let (key, canonical) = signing_fixture("20000229T074000Z");
    assert!(key
        .authorization("20000229T074000Z", &canonical, NAMES)
        .is_some());
    let (key, canonical) = signing_fixture(DATE);
    for invalid in ["UNSIGNED-PAYLOAD", &HASH[..63], &HASH.to_ascii_uppercase()] {
        assert!(key
            .authorization(DATE, &canonical.replace(HASH, invalid), NAMES)
            .is_none());
    }
    let mismatched = canonical.strip_suffix(HASH).unwrap().to_owned() + &"0".repeat(64);
    assert!(key.authorization(DATE, &mismatched, NAMES).is_none());
}

fn custody_fixture() -> CustodySnapshot {
    let id = *b"aaaaaa@a\x80aaaaaaa";
    let context = EnvelopeContext {
        identities: [id; 6],
        plaintext_len: 1,
        header: [7; 48],
    };
    CustodySnapshot {
        bearer_sha256: [2; 32],
        catalogue_reference: id,
        reference_id: id,
        identities: context.identities,
        map_id: id,
        plaintext_len: 1,
        plaintext_sha256: [3; 32],
        bucket: "fixture-bucket".into(),
        key: "media/61616161616140618061616161616161/61616161616140618061616161616161".into(),
        media_version: "v+/%".into(),
        page_versions: vec!["p+/%".into()],
        usable_until_unix: 1_900_000_000,
        header: context.header.to_vec(),
        envelope_record: wrap_scheme2(&[5; 32], &context, &[6; 32]).unwrap().to_vec(),
        page_hashes: vec![[4; 32]],
    }
}

#[test]
fn custody_pins_refuse_inconsistent_binding_metadata() {
    let pins = custody_fixture();
    assert!(pins.pins().is_some());
    let cases: [fn(&mut CustodySnapshot); 10] = [
        |s| s.catalogue_reference[0] ^= 1,
        |s| s.key.push('/'),
        |s| s.bucket.push('/'),
        |s| s.header[0] ^= 1,
        |s| s.identities[0][0] ^= 1,
        |s| {
            s.envelope_record.pop();
        },
        |s| s.media_version = "null".into(),
        |s| s.page_versions[0].push('\r'),
        |s| s.page_versions.clear(),
        |s| s.page_hashes.clear(),
    ];
    let accepted: Vec<_> = cases
        .into_iter()
        .enumerate()
        .filter_map(|(i, change)| {
            let mut snapshot = custody_fixture();
            change(&mut snapshot);
            snapshot.pins().map(|_| i)
        })
        .collect();
    assert!(
        accepted.is_empty(),
        "accepted malformed custody cases {accepted:?}"
    );
}

#[test]
fn custody_macs_require_a_bounded_bearer_and_bind_the_operation() {
    let secret = [3; 32];
    let resolve = signature(&secret, RESOLVE_PATH, "v4.payload.signature").unwrap();
    assert_ne!(
        Some(resolve),
        signature(&secret, LEASE_PATH, "v4.payload.signature")
    );
    assert_ne!(
        Some(resolve),
        signature(&secret, RESOLVE_PATH, "v4.payload.changed")
    );
    assert!(signature(&secret, RESOLVE_PATH, &"a".repeat(1800)).is_some());
    for invalid in [
        "",
        "v4.with\nnewline",
        "v4.with space",
        "v4.é",
        &"a".repeat(1801),
    ] {
        assert!(signature(&secret, RESOLVE_PATH, invalid).is_none());
    }
    assert!(signature(&secret[..31], RESOLVE_PATH, "v4.payload.signature").is_none());
    assert!(signature(&secret, "/not-a-custody-path", "v4.payload.signature").is_none());
}

#[test]
fn origin_output_requires_exact_authenticated_wire_length() {
    let mut origin = OriginSealer::new(PLAIN_CHUNK_SIZE + 1).unwrap();
    let header = origin.header();
    assert_eq!(format!("{:?}", origin.key()), "FileKey(<redacted>)");
    let mut wire = header.to_bytes().to_vec();
    wire.extend(
        origin
            .seal_chunk(&vec![7; PLAIN_CHUNK_SIZE as usize])
            .unwrap()
            .bytes,
    );
    wire.extend(origin.seal_chunk(&[8]).unwrap().bytes);
    let verified =
        VerifiedHeader::verify(&Profile::nntune(), KeyMode::Random, origin.key(), &header).unwrap();
    let reader = verified.reader(PLAIN_CHUNK_SIZE + 1).unwrap();
    assert!(reader.verify_all(wire.as_slice()).is_ok());
    let last = wire.len() - 1;
    assert!(reader.verify_all(&wire[..last]).is_err());
    wire[last] ^= 1;
    assert!(reader.verify_all(wire.as_slice()).is_err());
}

#[test]
fn layout_refuses_the_first_unrepresentable_ciphertext_size() {
    let available = u64::MAX - HEADER_SIZE;
    let plaintext =
        available / CIPHER_CHUNK_SIZE * PLAIN_CHUNK_SIZE + available % CIPHER_CHUNK_SIZE - TAG_SIZE;
    let last = plan_range(plaintext, plaintext - 1, plaintext - 1).unwrap();
    assert_eq!(last.ciphertext_end, u64::MAX);
    let all = plan_articles(plaintext, u64::MAX).unwrap();
    assert_eq!(all[0].ciphertext_offset + all[0].ciphertext_len, u64::MAX);
    assert!(plan_range(plaintext + 1, 0, 0).is_none());
    assert!(plan_articles(plaintext + 1, u64::MAX).is_none());
}
