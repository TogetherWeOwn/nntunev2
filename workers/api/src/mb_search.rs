//! `GET /v1/mb/search`: application-owned MusicBrainz search contract.
//!
//! Boundary: browser -> authenticated
//! application API -> private service binding -> QUALIFIED search backend,
//! with the application overlaying authoritative own/shared availability.
//! This module is the native, transport-free half: query validation, the
//! provider seam, and the strict response contract. It never talks to a
//! backend, never reads SQL, and never returns raw backend metadata.
//!
//! Status: no qualified authoritative provider exists yet (the snapshot
//! backend rejects `artist`/`recording`; the phase-0 gate stays NO-GO), so
//! the runtime provider is [`UnavailableProvider`], which always fails.
//! The route then answers an opaque 503 and the web client falls back to
//! library-only search. There is deliberately no fake empty success and no
//! fabricated availability. A real provider plugs in behind
//! [`MbSearchProvider`] once the prerequisite lands.
//!
//! Provider obligations (the contract a future implementation must meet):
//! - Own/shared membership and availability are applied BEFORE `offset` /
//!   `limit`; a provider must never fetch a capped raw page and filter it
//!   afterwards, nor page through a capped collection read.
//! - Unavailable rows are returned only when the request sets
//!   `include_unavailable`, which the handler grants only to a fresh DB
//!   admin role (concealed 404 otherwise).
//! - An MBID or membership row alone is not media readiness: `available`
//!   requires an authorized playable occurrence for recordings.
//! - Recording occurrences carry the existing library entry id and a
//!   separate server-derived `occurrence_key`; clients match on server
//!   occurrence identity, never on a job's `recording_mbids` array.
//!
//! [`build_response`] checks the structural response contract, not live
//! membership or readiness. Those remain the qualified provider's obligation.
//! Invalid shapes fail closed rather than being repaired.

use serde::Serialize;
use std::future::Future;

/// Largest page a caller may request (matches the catalogue page bound).
pub const MB_SEARCH_MAX_LIMIT: u32 = crate::catalogue::CATALOGUE_MAX_PAGE_SIZE as u32;
/// Page size when `limit` is absent.
pub const MB_SEARCH_DEFAULT_LIMIT: u32 = 20;
/// Largest `offset` a caller may request.
pub const MB_SEARCH_MAX_OFFSET: u32 = 1000;
/// Longest accepted `q`, in characters after percent-decoding and trim.
pub const MB_SEARCH_MAX_Q_CHARS: usize = 200;
/// Longest raw query string the parser looks at.
const MAX_RAW_QUERY_BYTES: usize = 4096;
/// Longest display text / identifier a provider row may carry.
const MAX_TEXT_CHARS: usize = 512;
const MAX_ID_CHARS: usize = 128;
/// Most playable occurrences one recording row may carry.
const MAX_OCCURRENCES: usize = 50;

/// The only entity types the route serves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum MbSearchType {
    Artist,
    Release,
    Recording,
}

impl MbSearchType {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "artist" => Some(Self::Artist),
            "release" => Some(Self::Release),
            "recording" => Some(Self::Recording),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Artist => "artist",
            Self::Release => "release",
            Self::Recording => "recording",
        }
    }
}

/// A validated search request. Constructed only by [`parse_query`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MbSearchQuery {
    pub q: String,
    pub kind: MbSearchType,
    pub limit: u32,
    pub offset: u32,
    pub include_unavailable: bool,
}

/// Why a query string was refused. The handler maps every variant to one
/// generic 422 validation envelope, so the variant never reaches a caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryError {
    TooLong,
    Malformed,
    UnknownKey,
    DuplicateKey,
    MissingQ,
    EmptyQ,
    BadType,
    BadLimit,
    BadOffset,
    BadFlag,
}

/// Percent-decode one query component (`+` is a space). Rejects bad
/// escapes and non-UTF-8 results.
fn decode_component(raw: &str) -> Result<String, QueryError> {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' => {
                let hi = bytes.get(i + 1).and_then(|b| (*b as char).to_digit(16));
                let lo = bytes.get(i + 2).and_then(|b| (*b as char).to_digit(16));
                match (hi, lo) {
                    (Some(hi), Some(lo)) => out.push((hi * 16 + lo) as u8),
                    _ => return Err(QueryError::Malformed),
                }
                i += 3;
            }
            byte => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8(out).map_err(|_| QueryError::Malformed)
}

fn parse_bounded_u32(raw: &str, max: u32) -> Option<u32> {
    if raw.is_empty() || raw.len() > 6 || !raw.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    raw.parse::<u32>().ok().filter(|n| *n <= max)
}

/// Parse and validate the raw query string (no leading `?`). Unknown and
/// duplicate keys are refused so the contract cannot silently grow
/// parameters.
pub fn parse_query(raw: &str) -> Result<MbSearchQuery, QueryError> {
    if raw.len() > MAX_RAW_QUERY_BYTES {
        return Err(QueryError::TooLong);
    }
    let mut q: Option<String> = None;
    let mut kind: Option<MbSearchType> = None;
    let mut limit: Option<u32> = None;
    let mut offset: Option<u32> = None;
    let mut include_unavailable: Option<bool> = None;
    for pair in raw.split('&').filter(|pair| !pair.is_empty()) {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        let key = decode_component(key)?;
        let value = decode_component(value)?;
        match key.as_str() {
            "q" => {
                if q.replace(value).is_some() {
                    return Err(QueryError::DuplicateKey);
                }
            }
            "type" => {
                let parsed = MbSearchType::parse(&value).ok_or(QueryError::BadType)?;
                if kind.replace(parsed).is_some() {
                    return Err(QueryError::DuplicateKey);
                }
            }
            "limit" => {
                let parsed =
                    parse_bounded_u32(&value, MB_SEARCH_MAX_LIMIT).ok_or(QueryError::BadLimit)?;
                if parsed == 0 {
                    return Err(QueryError::BadLimit);
                }
                if limit.replace(parsed).is_some() {
                    return Err(QueryError::DuplicateKey);
                }
            }
            "offset" => {
                let parsed =
                    parse_bounded_u32(&value, MB_SEARCH_MAX_OFFSET).ok_or(QueryError::BadOffset)?;
                if offset.replace(parsed).is_some() {
                    return Err(QueryError::DuplicateKey);
                }
            }
            "include_unavailable" => {
                let parsed = match value.as_str() {
                    "true" => true,
                    "false" => false,
                    _ => return Err(QueryError::BadFlag),
                };
                if include_unavailable.replace(parsed).is_some() {
                    return Err(QueryError::DuplicateKey);
                }
            }
            _ => return Err(QueryError::UnknownKey),
        }
    }
    let q = q.ok_or(QueryError::MissingQ)?;
    let q = q.trim().to_string();
    if q.is_empty() {
        return Err(QueryError::EmptyQ);
    }
    if q.chars().count() > MB_SEARCH_MAX_Q_CHARS || q.chars().any(char::is_control) {
        return Err(QueryError::Malformed);
    }
    Ok(MbSearchQuery {
        q,
        kind: kind.ok_or(QueryError::BadType)?,
        limit: limit.unwrap_or(MB_SEARCH_DEFAULT_LIMIT),
        offset: offset.unwrap_or(0),
        include_unavailable: include_unavailable.unwrap_or(false),
    })
}

/// What a provider receives: the validated query plus the authenticated
/// caller whose own/shared membership scopes the result. The caller id is
/// always session- or gate-derived, never a request parameter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MbSearchRequest {
    pub user_id: String,
    pub query: MbSearchQuery,
}

/// One playable recording occurrence: the existing library entry id and a
/// separate server-derived occurrence key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Occurrence {
    pub id: String,
    pub occurrence_key: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Availability {
    Available,
    Unavailable,
}

/// One provider row. Artists carry `name`; releases and recordings carry
/// `title` (the unused one is `None` and not serialized).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MbSearchResult {
    pub mbid: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub score: f32,
    pub availability: Availability,
    pub destination: Option<String>,
    pub occurrences: Vec<Occurrence>,
}

/// One already-paginated page from a provider (membership and
/// availability applied before pagination).
#[derive(Debug, Clone, PartialEq)]
pub struct MbSearchPage {
    pub results: Vec<MbSearchResult>,
}

/// Provider failure. Every variant is the same opaque 503 to the caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MbSearchError {
    /// No qualified authoritative provider is configured.
    Unavailable,
    /// The backend or its binding failed.
    Backend,
    /// The provider returned a row that violates the contract.
    Contract,
}

/// Native provider seam. Implementations own the private service-binding
/// call and the application's own/shared availability overlay.
pub trait MbSearchProvider {
    fn search(
        &self,
        request: &MbSearchRequest,
    ) -> impl Future<Output = Result<MbSearchPage, MbSearchError>>;
}

/// Runtime provider while no qualified backend exists: always fails.
#[derive(Debug, Clone, Copy, Default)]
pub struct UnavailableProvider;

impl MbSearchProvider for UnavailableProvider {
    async fn search(&self, _request: &MbSearchRequest) -> Result<MbSearchPage, MbSearchError> {
        Err(MbSearchError::Unavailable)
    }
}

/// Safe relative destination the web app may navigate to for a row, or
/// `None`. Artist `/artist/{mbid}`, release `/album/{mbid}`; recordings
/// never carry one.
pub fn destination_for(kind: MbSearchType, mbid: &str) -> Option<String> {
    if !crate::catalogue::is_catalogue_mbid(mbid) {
        return None;
    }
    match kind {
        MbSearchType::Artist => Some(format!("/artist/{mbid}")),
        MbSearchType::Release => Some(format!("/album/{mbid}")),
        MbSearchType::Recording => None,
    }
}

fn clean_text(text: &str, max: usize) -> bool {
    !text.trim().is_empty() && text.chars().count() <= max && !text.chars().any(char::is_control)
}

fn lowercase_mbid(mbid: &str) -> bool {
    crate::catalogue::is_catalogue_mbid(mbid) && mbid == mbid.to_ascii_lowercase()
}

fn validate_row(request: &MbSearchRequest, row: &MbSearchResult) -> Result<(), MbSearchError> {
    let kind = request.query.kind;
    if !lowercase_mbid(&row.mbid) || !row.score.is_finite() {
        return Err(MbSearchError::Contract);
    }
    // Exactly one display field, matching the entity type.
    let text_ok = match (kind, &row.name, &row.title) {
        (MbSearchType::Artist, Some(name), None) => clean_text(name, MAX_TEXT_CHARS),
        (MbSearchType::Release | MbSearchType::Recording, None, Some(title)) => {
            clean_text(title, MAX_TEXT_CHARS)
        }
        _ => false,
    };
    if !text_ok {
        return Err(MbSearchError::Contract);
    }
    match row.availability {
        Availability::Unavailable => {
            // Hidden by default; never a destination or a playable
            // occurrence for something the caller cannot use.
            if !request.query.include_unavailable
                || row.destination.is_some()
                || !row.occurrences.is_empty()
            {
                return Err(MbSearchError::Contract);
            }
        }
        Availability::Available => {
            if row.destination.is_some() && row.destination != destination_for(kind, &row.mbid) {
                return Err(MbSearchError::Contract);
            }
            if kind == MbSearchType::Recording {
                if row.occurrences.is_empty() || row.occurrences.len() > MAX_OCCURRENCES {
                    return Err(MbSearchError::Contract);
                }
                for (i, occ) in row.occurrences.iter().enumerate() {
                    if !clean_text(&occ.id, MAX_ID_CHARS)
                        || !clean_text(&occ.occurrence_key, MAX_ID_CHARS)
                        || occ.id == occ.occurrence_key
                        || row.occurrences[..i]
                            .iter()
                            .any(|prev| prev.occurrence_key == occ.occurrence_key)
                    {
                        return Err(MbSearchError::Contract);
                    }
                }
            } else if !row.occurrences.is_empty() {
                return Err(MbSearchError::Contract);
            }
        }
    }
    Ok(())
}

/// Validate a provider page against the contract and render the wire
/// body `{type, results}`. Fails closed on any violation, including a
/// page larger than the requested limit (pagination must happen in the
/// provider, after membership/availability).
pub fn build_response(
    request: &MbSearchRequest,
    page: &MbSearchPage,
) -> Result<serde_json::Value, MbSearchError> {
    if page.results.len() > request.query.limit as usize {
        return Err(MbSearchError::Contract);
    }
    for (i, row) in page.results.iter().enumerate() {
        validate_row(request, row)?;
        if page.results[..i].iter().any(|prev| prev.mbid == row.mbid) {
            return Err(MbSearchError::Contract);
        }
    }
    Ok(serde_json::json!({
        "type": request.query.kind,
        "results": page.results,
    }))
}

/// Run a provider and build the validated body. Any failure is an
/// [`MbSearchError`] the handler renders as the opaque 503.
pub async fn run<P: MbSearchProvider>(
    provider: &P,
    request: &MbSearchRequest,
) -> Result<serde_json::Value, MbSearchError> {
    let page = provider.search(request).await?;
    build_response(request, &page)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MBID: &str = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
    const MBID2: &str = "11111111-2222-3333-4444-555555555555";

    fn query(kind: MbSearchType, include_unavailable: bool) -> MbSearchRequest {
        MbSearchRequest {
            user_id: "u1".into(),
            query: MbSearchQuery {
                q: "x".into(),
                kind,
                limit: 2,
                offset: 0,
                include_unavailable,
            },
        }
    }

    fn artist(mbid: &str) -> MbSearchResult {
        MbSearchResult {
            mbid: mbid.into(),
            name: Some("Artist".into()),
            title: None,
            score: 0.9,
            availability: Availability::Available,
            destination: destination_for(MbSearchType::Artist, mbid),
            occurrences: vec![],
        }
    }

    fn recording(mbid: &str) -> MbSearchResult {
        MbSearchResult {
            mbid: mbid.into(),
            name: None,
            title: Some("Song".into()),
            score: 0.8,
            availability: Availability::Available,
            destination: None,
            occurrences: vec![Occurrence {
                id: "lib-1".into(),
                occurrence_key: "occ-1".into(),
            }],
        }
    }

    fn block_on<F: Future>(future: F) -> F::Output {
        let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
        let mut future = Box::pin(future);
        for _ in 0..1_000 {
            if let std::task::Poll::Ready(value) = future.as_mut().poll(&mut cx) {
                return value;
            }
        }
        panic!("future did not complete");
    }

    #[test]
    fn parses_minimal_and_full_queries() {
        let q = parse_query("q=Daft+Punk&type=artist").unwrap();
        assert_eq!(q.q, "Daft Punk");
        assert_eq!(q.kind, MbSearchType::Artist);
        assert_eq!((q.limit, q.offset, q.include_unavailable), (20, 0, false));
        let q = parse_query("type=recording&q=a%20b&limit=50&offset=1000&include_unavailable=true")
            .unwrap();
        assert_eq!(q.q, "a b");
        assert_eq!((q.limit, q.offset, q.include_unavailable), (50, 1000, true));
    }

    #[test]
    fn refuses_invalid_queries() {
        let long = format!("q={}&type=artist", "a".repeat(201));
        let huge = format!("q={}&type=artist", "a".repeat(2000));
        for raw in [
            "",
            "type=artist",
            "q=x",
            "q=&type=artist",
            "q=%20%20&type=artist",
            "q=x&type=track",
            "q=x&type=Artist",
            "q=x&type=artist&type=release",
            "q=x&q=y&type=artist",
            "q=x&type=artist&limit=0",
            "q=x&type=artist&limit=51",
            "q=x&type=artist&limit=-1",
            "q=x&type=artist&limit=1.5",
            "q=x&type=artist&offset=1001",
            "q=x&type=artist&offset=",
            "q=x&type=artist&include_unavailable=1",
            "q=x&type=artist&include_unavailable=TRUE",
            "q=x&type=artist&mode=raw",
            "q=%ZZ&type=artist",
            "q=%FF&type=artist",
            "q=a%00b&type=artist",
            "q=a%0Ab&type=artist",
            long.as_str(),
            huge.as_str(),
        ] {
            assert!(parse_query(raw).is_err(), "{raw:?} must be refused");
        }
    }

    #[test]
    fn unavailable_provider_never_yields_a_page() {
        let request = query(MbSearchType::Artist, false);
        assert_eq!(
            block_on(run(&UnavailableProvider, &request)),
            Err(MbSearchError::Unavailable)
        );
    }

    struct Fixed(Vec<MbSearchResult>);
    impl MbSearchProvider for Fixed {
        async fn search(&self, _r: &MbSearchRequest) -> Result<MbSearchPage, MbSearchError> {
            Ok(MbSearchPage {
                results: self.0.clone(),
            })
        }
    }

    #[test]
    fn renders_strict_wire_shape() {
        let request = query(MbSearchType::Recording, false);
        let body = block_on(run(&Fixed(vec![recording(MBID)]), &request)).unwrap();
        assert_eq!(
            body,
            serde_json::json!({
                "type": "recording",
                "results": [{
                    "mbid": MBID,
                    "title": "Song",
                    "score": 0.8_f32,
                    "availability": "available",
                    "destination": null,
                    "occurrences": [{"id": "lib-1", "occurrence_key": "occ-1"}],
                }],
            })
        );
        let request = query(MbSearchType::Artist, false);
        let body = block_on(run(&Fixed(vec![artist(MBID)]), &request)).unwrap();
        assert_eq!(body["results"][0]["destination"], format!("/artist/{MBID}"));
        assert!(body["results"][0].get("title").is_none());
    }

    #[test]
    fn rejects_contract_violations() {
        let artist_req = query(MbSearchType::Artist, false);
        let mut bad = Vec::new();
        // Unavailable row without admin include.
        let mut r = artist(MBID);
        r.availability = Availability::Unavailable;
        r.destination = None;
        bad.push(r);
        // Forged destination.
        let mut r = artist(MBID);
        r.destination = Some("https://evil.example/".into());
        bad.push(r);
        let mut r = artist(MBID);
        r.destination = Some(format!("/album/{MBID}"));
        bad.push(r);
        // Bad ids, score, text.
        bad.push(artist("not-an-mbid"));
        bad.push(artist(&MBID.to_ascii_uppercase()));
        let mut r = artist(MBID);
        r.score = f32::NAN;
        bad.push(r);
        let mut r = artist(MBID);
        r.name = Some(" ".into());
        bad.push(r);
        let mut r = artist(MBID);
        r.title = Some("both".into());
        bad.push(r);
        // Occurrences on a non-recording.
        let mut r = artist(MBID);
        r.occurrences = recording(MBID).occurrences;
        bad.push(r);
        for row in bad {
            assert_eq!(
                block_on(run(&Fixed(vec![row.clone()]), &artist_req)),
                Err(MbSearchError::Contract),
                "{row:?}"
            );
        }
        // Over-limit page and duplicate MBIDs.
        let rows = vec![
            artist(MBID),
            artist(MBID2),
            artist("33333333-2222-3333-4444-555555555555"),
        ];
        assert_eq!(
            block_on(run(&Fixed(rows), &artist_req)),
            Err(MbSearchError::Contract)
        );
        assert_eq!(
            block_on(run(&Fixed(vec![artist(MBID), artist(MBID)]), &artist_req)),
            Err(MbSearchError::Contract)
        );
    }

    #[test]
    fn recording_occurrence_rules() {
        let req = query(MbSearchType::Recording, false);
        let mut cases = Vec::new();
        let mut r = recording(MBID);
        r.occurrences.clear();
        cases.push(r);
        let mut r = recording(MBID);
        r.occurrences[0].occurrence_key = r.occurrences[0].id.clone();
        cases.push(r);
        let mut r = recording(MBID);
        r.occurrences[0].id = String::new();
        cases.push(r);
        let mut r = recording(MBID);
        r.occurrences.push(Occurrence {
            id: "lib-2".into(),
            occurrence_key: "occ-1".into(),
        });
        cases.push(r);
        let mut r = recording(MBID);
        r.destination = Some(format!("/album/{MBID}"));
        cases.push(r);
        for row in cases {
            assert_eq!(
                block_on(run(&Fixed(vec![row.clone()]), &req)),
                Err(MbSearchError::Contract),
                "{row:?}"
            );
        }
    }

    #[test]
    fn admin_include_allows_unavailable_without_playable_data() {
        let req = query(MbSearchType::Recording, true);
        let mut r = recording(MBID);
        r.availability = Availability::Unavailable;
        r.occurrences.clear();
        let body = block_on(run(&Fixed(vec![r.clone()]), &req)).unwrap();
        assert_eq!(body["results"][0]["availability"], "unavailable");
        // Still no playable occurrences on an unavailable row.
        r.occurrences = recording(MBID).occurrences;
        assert_eq!(
            block_on(run(&Fixed(vec![r]), &req)),
            Err(MbSearchError::Contract)
        );
    }
}
