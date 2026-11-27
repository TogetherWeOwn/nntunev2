//! Internal contract for reading the durable result of a scoped completion.

/// Exact scope of the authorized session and its checkpoint (final for completion).
#[derive(Clone, Copy)]
pub struct CompletionScope<'a> {
    pub environment: [u8; 16],
    pub library: [u8; 16],
    pub principal: &'a str,
    pub session: &'a str,
    pub fingerprint: &'a [u8; 32],
    pub seal: [u8; 16],
    pub revision: i64,
    pub offset: i64,
}

impl CompletionScope<'_> {
    pub(crate) fn valid_final_checkpoint(&self) -> bool {
        (1..=self.offset).contains(&self.revision)
            && (valid_uuid_text(self.session)
                || (self.session.len() == 32
                    && self.session.bytes().all(|byte| byte.is_ascii_hexdigit())))
    }
}

pub(crate) fn valid_uuid_text(id: &str) -> bool {
    id.len() == 36 && crate::queue::validate_provider_id(id)
}

pub(crate) fn valid_version_pin(id: &str) -> bool {
    id != "null"
        && (1..=1024).contains(&id.len())
        && id
            .bytes()
            .all(|byte| byte.is_ascii_graphic() && byte != b',')
}

#[cfg(any(target_arch = "wasm32", test))]
pub(crate) fn valid_readback_framing(length: Option<u64>, transfer_encoding: bool) -> bool {
    length.is_none() || !transfer_encoding
}

#[cfg(any(target_arch = "wasm32", test))]
pub(crate) fn valid_compact_uuid_v4(id: &str) -> bool {
    let bytes = id.as_bytes();
    bytes.len() == 32
        && bytes[12] == b'4'
        && matches!(bytes[16], b'8'..=b'b')
        && bytes.iter().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

#[cfg(any(target_arch = "wasm32", test))]
pub(crate) fn valid_media_key(key: &str) -> bool {
    key.strip_prefix("media/")
        .and_then(|path| path.split_once('/'))
        .is_some_and(|(object, generation)| {
            valid_compact_uuid_v4(object) && valid_compact_uuid_v4(generation)
        })
}

/// Immutable pins from the committed completion row and its inventory. IDs
/// are canonical PostgreSQL UUID text. Pins confer no storage access.
pub struct CommittedOutcome {
    pub map: String,
    pub object: String,
    pub generation: String,
    pub configuration: String,
    pub root_version: String,
    pub media_version: String,
    pub page_versions: Vec<String>,
}

pub(crate) fn uuid_text(id: [u8; 16]) -> String {
    let mut id = hex::encode(id);
    for boundary in [20, 16, 12, 8] {
        id.insert(boundary, '-');
    }
    id
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readback_refuses_conflicting_framing_headers() {
        assert!(valid_readback_framing(Some(42), false));
        assert!(!valid_readback_framing(Some(42), true));
    }

    #[test]
    fn final_checkpoint_rejects_impossible_coordinates_and_malformed_session_ids() {
        let mut scope = CompletionScope {
            environment: [1; 16],
            library: [2; 16],
            principal: "owner",
            session: "00000000-0000-4000-8000-000000000001",
            fingerprint: &[3; 32],
            seal: [4; 16],
            revision: 1,
            offset: 10,
        };
        assert!(scope.valid_final_checkpoint());
        scope.session = "00000000000040008000000000000001";
        assert!(scope.valid_final_checkpoint());
        for revision in [-1, 0, 11] {
            scope.revision = revision;
            assert!(!scope.valid_final_checkpoint());
        }
        scope.revision = 1;
        scope.session = "invalid";
        assert!(!scope.valid_final_checkpoint());
    }
}
