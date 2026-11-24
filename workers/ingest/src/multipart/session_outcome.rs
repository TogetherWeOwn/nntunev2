//! Internal contract for reading the durable result of a scoped completion.

/// Exact scope of the authorized session and its final checkpoint.
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
