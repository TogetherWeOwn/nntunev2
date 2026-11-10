//! Pure, bounded extraction of Subsonic credentials from the HTTP query
//! string and an optional `application/x-www-form-urlencoded` body.
//!
//! The caller has already checked the content type and read the body under
//! its own transport cap; this module only parses. No I/O, no allocation of
//! unbounded state, no logging.
//!
//! Rules (all fail closed, before a [`Credentials`] value exists):
//!
//! - Keys are percent/plus-decoded before comparison, so `%61piKey` and
//!   `api%4Bey` are the credential `apiKey`. A credential key seen twice,
//!   within one source or across query and form, is rejected: first-wins or
//!   last-wins would let a smuggled copy disagree with what the core
//!   authenticator checks. An array-style key (`u[]`, `apiKey[x]`) whose
//!   base name is a credential key is rejected for the same reason.
//! - A malformed percent escape or non-UTF-8 decoded text in any key, or in
//!   the value of a credential key, is an error (never lossily repaired).
//!   Values of non-credential parameters are not decoded here.
//! - Presence is preserved: `p` and `p=` both yield `Some("")`, which the
//!   production core refuses as `Unsupported`, even alongside another mode.
//! - Raw payload size, parameter count, key length and credential value
//!   length are bounded.
//!
//! Errors carry only a class, never input text; `Credentials` has a
//! presence-only `Debug`.

use std::fmt;

use nntune_subsonic::auth::Credentials;

/// Maximum raw bytes accepted per source (query or form body).
pub const MAX_SOURCE_BYTES: usize = 256 * 1024;
/// Maximum `&`-separated non-empty parameters per source.
pub const MAX_PARAMS_PER_SOURCE: usize = 1024;
/// Maximum raw (still encoded) bytes of one parameter key.
pub const MAX_KEY_BYTES: usize = 256;
/// Maximum raw (still encoded) bytes of one credential value.
pub const MAX_CREDENTIAL_VALUE_BYTES: usize = 1024;

/// Payload-free failure classes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CredentialParseError {
    /// A source exceeded [`MAX_SOURCE_BYTES`].
    SourceTooLarge,
    /// A source exceeded [`MAX_PARAMS_PER_SOURCE`].
    TooManyParams,
    /// A key exceeded [`MAX_KEY_BYTES`].
    KeyTooLong,
    /// A credential value exceeded [`MAX_CREDENTIAL_VALUE_BYTES`].
    ValueTooLong,
    /// Invalid percent escape or non-UTF-8 text.
    MalformedEncoding,
    /// A credential parameter appeared more than once (any source), or in
    /// array form.
    DuplicateCredential,
}

impl fmt::Display for CredentialParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::SourceTooLarge => "request parameters too large",
            Self::TooManyParams => "too many request parameters",
            Self::KeyTooLong => "parameter name too long",
            Self::ValueTooLong => "credential parameter too long",
            Self::MalformedEncoding => "malformed parameter encoding",
            Self::DuplicateCredential => "duplicate credential parameter",
        })
    }
}

impl std::error::Error for CredentialParseError {}

#[derive(Clone, Copy)]
enum Slot {
    ApiKey,
    Username,
    Password,
    Token,
    Salt,
}

impl Slot {
    fn from_key(key: &str) -> Option<Slot> {
        match key {
            "apiKey" => Some(Slot::ApiKey),
            "u" => Some(Slot::Username),
            "p" => Some(Slot::Password),
            "t" => Some(Slot::Token),
            "s" => Some(Slot::Salt),
            _ => None,
        }
    }
}

/// Parse credentials from an optional raw query string (without the leading
/// `?`) and an optional raw form body. Both sources are scanned fully so a
/// duplicate in either is always caught; absent sources contribute nothing.
pub fn parse_credentials(
    query: Option<&str>,
    form_body: Option<&[u8]>,
) -> Result<Credentials, CredentialParseError> {
    let mut creds = Credentials::default();
    let mut seen = [false; 5];
    if let Some(q) = query {
        scan(q.as_bytes(), &mut creds, &mut seen)?;
    }
    if let Some(b) = form_body {
        scan(b, &mut creds, &mut seen)?;
    }
    Ok(creds)
}

fn scan(
    raw: &[u8],
    creds: &mut Credentials,
    seen: &mut [bool; 5],
) -> Result<(), CredentialParseError> {
    if raw.len() > MAX_SOURCE_BYTES {
        return Err(CredentialParseError::SourceTooLarge);
    }
    let mut count = 0usize;
    for pair in raw.split(|b| *b == b'&') {
        if pair.is_empty() {
            continue;
        }
        count += 1;
        if count > MAX_PARAMS_PER_SOURCE {
            return Err(CredentialParseError::TooManyParams);
        }
        let (raw_key, raw_value) = match pair.iter().position(|b| *b == b'=') {
            Some(i) => (&pair[..i], &pair[i + 1..]),
            None => (pair, &pair[pair.len()..]),
        };
        if raw_key.len() > MAX_KEY_BYTES {
            return Err(CredentialParseError::KeyTooLong);
        }
        let key = decode(raw_key)?;
        // Array-style names (`u[]`) are judged by their base name.
        let base = key.split('[').next().unwrap_or("");
        let Some(slot) = Slot::from_key(base) else {
            continue;
        };
        if base.len() != key.len() {
            return Err(CredentialParseError::DuplicateCredential);
        }
        let idx = slot as usize;
        if seen[idx] {
            return Err(CredentialParseError::DuplicateCredential);
        }
        seen[idx] = true;
        if raw_value.len() > MAX_CREDENTIAL_VALUE_BYTES {
            return Err(CredentialParseError::ValueTooLong);
        }
        let value = Some(decode(raw_value)?);
        match slot {
            Slot::ApiKey => creds.api_key = value,
            Slot::Username => creds.username = value,
            Slot::Password => creds.password = value,
            Slot::Token => creds.token = value,
            Slot::Salt => creds.salt = value,
        }
    }
    Ok(())
}

/// Form-urlencoded decode: `+` is a space, `%XX` must be two hex digits,
/// result must be UTF-8.
fn decode(raw: &[u8]) -> Result<String, CredentialParseError> {
    let mut out = Vec::with_capacity(raw.len());
    let mut i = 0;
    while i < raw.len() {
        match raw[i] {
            b'+' => out.push(b' '),
            b'%' => {
                let hi = raw.get(i + 1).copied().and_then(hex);
                let lo = raw.get(i + 2).copied().and_then(hex);
                match (hi, lo) {
                    (Some(h), Some(l)) => out.push((h << 4) | l),
                    _ => return Err(CredentialParseError::MalformedEncoding),
                }
                i += 2;
            }
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8(out).map_err(|_| CredentialParseError::MalformedEncoding)
}

fn hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(query: &str) -> Result<Credentials, CredentialParseError> {
        parse_credentials(Some(query), None)
    }

    #[test]
    fn valid_sources_and_absence() {
        let c = parse_credentials(None, None).unwrap();
        assert!(c.api_key.is_none() && c.username.is_none() && c.password.is_none());
        let c = q("u=al+ice&t=abc&s=xyz&v=1.16&c=app").unwrap();
        assert_eq!(c.username.as_deref(), Some("al ice"));
        assert_eq!(c.token.as_deref(), Some("abc"));
        assert_eq!(c.salt.as_deref(), Some("xyz"));
        assert!(c.api_key.is_none() && c.password.is_none());
        let c = parse_credentials(Some("c=app&id=3"), Some(b"apiKey=k%2Bk")).unwrap();
        assert_eq!(c.api_key.as_deref(), Some("k+k"));
        // Split across sources without duplication.
        let c = parse_credentials(Some("u=bob"), Some(b"p=enc%3A6162")).unwrap();
        assert_eq!(c.username.as_deref(), Some("bob"));
        assert_eq!(c.password.as_deref(), Some("enc:6162"));
        // Empty pairs are skipped and values may contain '='.
        let c = q("&&u=a=b&").unwrap();
        assert_eq!(c.username.as_deref(), Some("a=b"));
    }

    #[test]
    fn empty_password_presence_is_preserved() {
        assert_eq!(q("p=").unwrap().password.as_deref(), Some(""));
        assert_eq!(q("p").unwrap().password.as_deref(), Some(""));
        let c = parse_credentials(None, Some(b"u=a&p=")).unwrap();
        assert_eq!(c.password.as_deref(), Some(""));
        assert_eq!(q("u=").unwrap().username.as_deref(), Some(""));
    }

    #[test]
    fn duplicates_within_one_source_rejected() {
        for s in [
            "u=a&u=b",
            "p=&p=x",
            "apiKey=a&apiKey=a",
            "t=1&s=2&t=1",
            "s=1&s",
        ] {
            assert_eq!(
                q(s).unwrap_err(),
                CredentialParseError::DuplicateCredential,
                "{s}"
            );
            assert_eq!(
                parse_credentials(None, Some(s.as_bytes())).unwrap_err(),
                CredentialParseError::DuplicateCredential,
                "{s}"
            );
        }
    }

    #[test]
    fn duplicates_across_query_and_form_rejected() {
        for (a, b) in [
            ("u=a", "u=b"),
            ("p=", "p=secret"),
            ("apiKey=k", "apiKey=k"),
            ("t=1", "t=1"),
            ("s=1", "s=2"),
        ] {
            assert_eq!(
                parse_credentials(Some(a), Some(b.as_bytes())).unwrap_err(),
                CredentialParseError::DuplicateCredential
            );
            assert_eq!(
                parse_credentials(Some(b), Some(a.as_bytes())).unwrap_err(),
                CredentialParseError::DuplicateCredential
            );
        }
    }

    #[test]
    fn encoded_key_aliases_count_as_duplicates() {
        assert_eq!(
            parse_credentials(Some("u=a"), Some(b"%75=b")).unwrap_err(),
            CredentialParseError::DuplicateCredential
        );
        assert_eq!(
            q("apiKey=a&%61pi%4bey=b").unwrap_err(),
            CredentialParseError::DuplicateCredential
        );
        assert_eq!(
            q("p=x&%70=y").unwrap_err(),
            CredentialParseError::DuplicateCredential
        );
        assert_eq!(
            q("%73=1&%73").unwrap_err(),
            CredentialParseError::DuplicateCredential
        );
        // Encoded key alone is the credential.
        assert_eq!(q("%75=a").unwrap().username.as_deref(), Some("a"));
        // Array-style names are never silently ignored.
        for s in ["u[]=a", "apiKey[x]=k", "%75%5B%5D=a", "p[=x"] {
            assert_eq!(
                q(s).unwrap_err(),
                CredentialParseError::DuplicateCredential,
                "{s}"
            );
        }
        // Case matters and non-credential keys are ignored.
        assert!(q("U=a&P=b&user=c").unwrap().username.is_none());
    }

    #[test]
    fn malformed_encodings_fail_closed() {
        for s in [
            "u=%",
            "u=%4",
            "u=%zz",
            "u=%G1",
            "u=a%2",
            "%=1",
            "%7=1",
            "%zz=1&u=a",
            "u=%ff",
            "u=%c3%28",
            "%ff=1",
        ] {
            assert_eq!(
                q(s).unwrap_err(),
                CredentialParseError::MalformedEncoding,
                "{s}"
            );
        }
        assert_eq!(
            parse_credentials(None, Some(b"u=\xff")).unwrap_err(),
            CredentialParseError::MalformedEncoding
        );
        assert_eq!(
            parse_credentials(Some("c=x"), Some(b"p=%e2%82")).unwrap_err(),
            CredentialParseError::MalformedEncoding
        );
        // Valid multi-byte UTF-8 decodes.
        assert_eq!(
            q("u=%e2%82%ac").unwrap().username.as_deref(),
            Some("\u{20ac}")
        );
        // Non-credential values are not decoded, so they cannot fail parsing.
        assert!(q("id=%zz").unwrap().username.is_none());
    }

    #[test]
    fn bounds_are_enforced() {
        let big = "a".repeat(MAX_SOURCE_BYTES + 1);
        assert_eq!(q(&big).unwrap_err(), CredentialParseError::SourceTooLarge);
        assert_eq!(
            parse_credentials(None, Some(big.as_bytes())).unwrap_err(),
            CredentialParseError::SourceTooLarge
        );
        let many = vec!["x=1"; MAX_PARAMS_PER_SOURCE + 1].join("&");
        assert_eq!(q(&many).unwrap_err(), CredentialParseError::TooManyParams);
        let ok = vec!["x=1"; MAX_PARAMS_PER_SOURCE].join("&");
        assert!(q(&ok).is_ok());
        // Counts are per source and empty pairs are free.
        assert!(parse_credentials(Some(&ok), Some(ok.as_bytes())).is_ok());
        assert!(q(&"&".repeat(MAX_PARAMS_PER_SOURCE * 2)).is_ok());
        let key = "k".repeat(MAX_KEY_BYTES + 1);
        assert_eq!(
            q(&format!("{key}=1")).unwrap_err(),
            CredentialParseError::KeyTooLong
        );
        let long = "v".repeat(MAX_CREDENTIAL_VALUE_BYTES + 1);
        assert_eq!(
            q(&format!("p={long}")).unwrap_err(),
            CredentialParseError::ValueTooLong
        );
        let edge = "v".repeat(MAX_CREDENTIAL_VALUE_BYTES);
        assert_eq!(
            q(&format!("p={edge}")).unwrap().password.as_deref(),
            Some(edge.as_str())
        );
        // Long non-credential values are fine.
        assert!(q(&format!("id={long}")).is_ok());
    }

    #[test]
    fn errors_and_credentials_do_not_leak_secrets() {
        let canary = "x".repeat(32);
        let secret = canary.as_str();
        for r in [
            q(&format!("p={secret}&p={secret}")).unwrap_err(),
            q(&format!("p={secret}%zz")).unwrap_err(),
            parse_credentials(
                Some(&format!("t={secret}")),
                Some(format!("t={secret}").as_bytes()),
            )
            .unwrap_err(),
        ] {
            assert!(!format!("{r}").contains(secret));
            assert!(!format!("{r:?}").contains(secret));
        }
        let c = q(&format!("u=alice&p={secret}")).unwrap();
        let dbg = format!("{c:?}");
        assert!(!dbg.contains(secret) && !dbg.contains("alice"));
    }
}
