//! Acceptance tests for public cover-art URL generation.
//!
//! Public artwork URLs are optional until a bucket's public endpoint is
//! configured. Configured endpoints must be used as the URL origin while the
//! object key remains the path. These tests exercise the public URL helper;
//! they do not require Cloudflare credentials, a live bucket, or staging.

use nntune_mb::caa_url_for_art;

const ART_KEY: &str = "art/ab/cd/0123456789abcdef-500";

#[test]
fn missing_public_host_does_not_emit_a_cover_art_url() {
    assert_eq!(caa_url_for_art("nntunev2-art-staging", ART_KEY, None), None);
    assert_eq!(caa_url_for_art("nntunev2-art-prod", ART_KEY, None), None);
}

#[test]
fn configured_public_hosts_emit_bucket_specific_urls() {
    let staging = caa_url_for_art(
        "nntunev2-art-staging",
        ART_KEY,
        Some("https://art-staging.example.com/"),
    );
    let production = caa_url_for_art(
        "nntunev2-art-prod",
        ART_KEY,
        Some("https://art.example.com"),
    );

    assert_eq!(
        staging.as_deref(),
        Some("https://art-staging.example.com/art/ab/cd/0123456789abcdef-500")
    );
    assert_eq!(
        production.as_deref(),
        Some("https://art.example.com/art/ab/cd/0123456789abcdef-500")
    );
}
