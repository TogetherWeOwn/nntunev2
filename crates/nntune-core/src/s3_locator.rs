//! Private metadata for encrypted S3 media. No provider or serving path.

use std::fmt::{self, Write as _};

use serde::{Deserialize, Serialize};

use crate::{media::ciphertext_size, media_envelope::EnvelopeContext};

const MAX_LOCATOR_JSON: usize = 8 * 1024;
const MAX_BINDING_BYTES: usize = 1024;

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum S3Binding {
    /// The actual returned version ID, never a latest or null version selector.
    VersionId(String),
    /// One opaque, strong entity-tag for a nonversioned object. Not a digest.
    Etag(String),
}

#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct S3Locator {
    bucket: String,
    key: String,
    binding: S3Binding,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum S3LocatorError {
    Shape,
    Bucket,
    Key,
    Binding,
}

impl S3Locator {
    /// Validate shape only. The trusted producer must assure version provenance,
    /// configuration ownership and durable key non-reuse.
    pub fn new(bucket: String, key: String, binding: S3Binding) -> Result<Self, S3LocatorError> {
        if !valid_bucket(&bucket) {
            return Err(S3LocatorError::Bucket);
        }
        if !key
            .strip_prefix("media/")
            .and_then(|path| path.split_once('/'))
            .is_some_and(|(object, generation)| valid_key_id(object) && valid_key_id(generation))
        {
            return Err(S3LocatorError::Key);
        }
        let valid_binding = match &binding {
            // A comma can separate combined x-amz-version-id header values.
            S3Binding::VersionId(id) => id != "null" && opaque_token(id) && !id.contains(','),
            S3Binding::Etag(tag) => tag
                .strip_prefix('"')
                .and_then(|tag| tag.strip_suffix('"'))
                .is_some_and(|tag| opaque_token(tag) && !tag.contains('"')),
        };
        if !valid_binding {
            return Err(S3LocatorError::Binding);
        }
        Ok(Self {
            bucket,
            key,
            binding,
        })
    }

    /// Parse exactly one bounded JSON object; do not accept positional fields,
    /// duplicate fields, alternate bindings or provider configuration overrides.
    pub fn parse(bytes: &[u8]) -> Result<Self, S3LocatorError> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Fields {
            bucket: String,
            key: String,
            binding: S3Binding,
        }
        if bytes.len() > MAX_LOCATOR_JSON || bytes.trim_ascii_start().first() != Some(&b'{') {
            return Err(S3LocatorError::Shape);
        }
        let fields: Fields = serde_json::from_slice(bytes).map_err(|_| S3LocatorError::Shape)?;
        Self::new(fields.bucket, fields.key, fields.binding)
    }

    pub fn bucket(&self) -> &str {
        &self.bucket
    }

    pub fn key(&self) -> &str {
        &self.key
    }

    pub fn binding(&self) -> &S3Binding {
        &self.binding
    }
}

pub fn valid_bucket(bucket: &str) -> bool {
    let mut labels = bucket.split('.');
    (3..=63).contains(&bucket.len())
        && labels.clone().all(|label| {
            !label.is_empty()
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'-'))
        })
        && !(labels.clone().count() == 4
            && labels.all(|label| label.bytes().all(|b| b.is_ascii_digit())))
}

fn opaque_token(token: &str) -> bool {
    (1..=MAX_BINDING_BYTES).contains(&token.len()) && token.bytes().all(|b| b.is_ascii_graphic())
}

fn valid_key_id(id: &str) -> bool {
    id.len() == 32
        && id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        && id.as_bytes()[12] == b'4'
        && matches!(id.as_bytes()[16], b'8'..=b'9' | b'a'..=b'b')
}

fn context_media_key(identities: &[[u8; 16]; 5]) -> Option<String> {
    if identities
        .iter()
        .any(|id| id[6] >> 4 != 4 || id[8] >> 6 != 2)
    {
        return None;
    }
    let mut key = String::from("media");
    for id in &identities[2..4] {
        key.push('/');
        for byte in id {
            write!(&mut key, "{byte:02x}").ok()?;
        }
    }
    Some(key)
}

/// Trusted media expectations stored alongside, not inside, the locator.
/// Neither these fields nor an ETag authenticate any provider response.
#[derive(Clone, PartialEq, Eq)]
pub struct S3MediaMetadata {
    locator: S3Locator,
    // Environment, library, object, sealing generation, provider configuration.
    identities: [[u8; 16]; 5],
    plaintext_len: u64,
    ciphertext_len: u64,
    header: [u8; 48],
}

impl fmt::Debug for S3MediaMetadata {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("S3MediaMetadata").finish_non_exhaustive()
    }
}

impl S3MediaMetadata {
    /// Validate the target bucket shape and the exact key for the trusted
    /// context before a provider returns a version. The caller establishes
    /// bucket ownership and authorization independently.
    pub fn matches_target(bucket: &str, key: &str, context: &EnvelopeContext) -> bool {
        let [_, identities @ ..] = &context.identities;
        valid_bucket(bucket) && context_media_key(identities).as_deref() == Some(key)
    }

    /// Build from independently trusted context, not a returned tag, length,
    /// or envelope. Full envelope authentication and readback remain separate.
    pub fn from_trusted_context(locator: S3Locator, context: &EnvelopeContext) -> Option<Self> {
        let [_, identities @ ..] = context.identities;
        if context_media_key(&identities).as_deref() != Some(locator.key()) {
            return None;
        }
        Some(Self {
            locator,
            identities,
            plaintext_len: context.plaintext_len,
            ciphertext_len: ciphertext_size(context.plaintext_len)?,
            header: context.header,
        })
    }

    pub fn locator(&self) -> &S3Locator {
        &self.locator
    }

    pub fn plaintext_len(&self) -> u64 {
        self.plaintext_len
    }

    pub fn ciphertext_len(&self) -> u64 {
        self.ciphertext_len
    }

    /// Compare immutable context only. Root-version changes from authorized
    /// rewraps do not change the association; this check does not authorize one.
    pub fn matches_context(&self, context: &EnvelopeContext) -> bool {
        let [_, identities @ ..] = context.identities;
        self.identities == identities
            && self.plaintext_len == context.plaintext_len
            && self.header == context.header
    }
}
