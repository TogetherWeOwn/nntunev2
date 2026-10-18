//! Regression pins: `FieldRejection::PiiValue` and `FieldRejection::IpValue`
//! exact-variant mapping in `trace::check_str_value`.
//!
//! Contract under test (implementation unchanged):
//!
//! - Email-shaped values yield exactly `PiiValue`; IPv4- and IPv6-shaped
//!   values yield exactly `IpValue` (not `SecretValue`, not `BannedKey`).
//! - Rejected values are never echoed in the `Debug` rendering.
//! - Near-miss shapes (`user@` without domain, `999.999.999.999`) do not
//!   take these arms and insert cleanly.
//!
//! All fixtures are synthetic and offline: no network, no telemetry.

use nntune_stream::trace::{FieldRejection, LogFields};

#[test]
fn email_shaped_values_yield_exactly_pii_value() {
    for value in [
        "a@example.com",
        "user+tag@mail.example.co",
        "first.last@example.org",
    ] {
        let mut fields = LogFields::new();
        let err = fields
            .insert_str("contact", value)
            .expect_err("email-shaped value must be rejected");
        assert_eq!(
            err,
            FieldRejection::PiiValue,
            "email-shaped value {value:?} must yield PiiValue"
        );
    }
}

#[test]
fn ip_shaped_values_yield_exactly_ip_value() {
    // TEST-NET documentation ranges only: the public-text lint bans
    // RFC-1918 addresses (e.g. 10/8) in tracked files.
    for value in ["192.0.2.1", "198.51.100.23", "203.0.113.45"] {
        let mut fields = LogFields::new();
        let err = fields
            .insert_str("host", value)
            .expect_err("IPv4-shaped value must be rejected");
        assert_eq!(
            err,
            FieldRejection::IpValue,
            "IPv4-shaped value {value:?} must yield IpValue"
        );
    }
    for value in [
        "::1",
        "2001:db8::1",
        "2001:0db8:0000:0000:0000:0000:0000:0001",
    ] {
        let mut fields = LogFields::new();
        let err = fields
            .insert_str("host", value)
            .expect_err("IPv6-shaped value must be rejected");
        assert_eq!(
            err,
            FieldRejection::IpValue,
            "IPv6-shaped value {value:?} must yield IpValue"
        );
    }
}

#[test]
fn pii_and_ip_rejections_never_echo_values() {
    for (key, value) in [
        ("contact", "a@example.com"),
        ("host", "192.0.2.1"),
        ("host", "2001:db8::1"),
    ] {
        let mut fields = LogFields::new();
        let err = fields
            .insert_str(key, value)
            .expect_err("fixture value must be rejected");
        let rendered = format!("{err:?}");
        assert!(
            !rendered.contains(value),
            "rejection must not echo {value:?}, got {rendered:?}"
        );
        // Distinctive substrings must not leak either (local part, domain,
        // octets, hextets).
        for fragment in value
            .split(|c: char| !c.is_ascii_alphanumeric())
            .filter(|part| part.len() >= 2)
        {
            // Short generic fragments (`db`, `co`) also appear in variant
            // names by coincidence; only assert on fragments distinctive
            // enough to identify the rejected value.
            if fragment.len() >= 4 {
                assert!(
                    !rendered.contains(fragment),
                    "rejection must not echo fragment {fragment:?} of {value:?}, got {rendered:?}"
                );
            }
        }
    }
    // Variant names themselves carry no payload.
    assert_eq!(format!("{:?}", FieldRejection::PiiValue), "PiiValue");
    assert_eq!(format!("{:?}", FieldRejection::IpValue), "IpValue");
}

#[test]
fn near_miss_shapes_do_not_take_pii_or_ip_arms() {
    // `user@` has no domain; `999.999.999.999` has out-of-range octets.
    // Neither may take the Pii/Ip arms, so both inserts succeed.
    for (key, value) in [
        ("contact", "user@"),
        ("contact", "user@nodot"),
        ("contact", "not-an-email"),
        ("host", "999.999.999.999"),
        ("host", "256.0.2.1"),
        ("host", "1.2.3"),
    ] {
        let mut fields = LogFields::new();
        assert!(
            fields.insert_str(key, value).is_ok(),
            "near-miss shape {value:?} must not take the Pii/Ip arms"
        );
    }
}
