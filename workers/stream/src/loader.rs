//! Pure live-loader core: SSRF dial-plan vetting, capacity derivation and
//! fixed error mapping. First slice only; the runtime loader stays stopped.
//!
//! Four pure functions over caller-supplied inputs, with closed
//! request/response types and the pinned bounds table:
//!
//! - [`vet_dial_plan`]: vet-ALL dial-plan check over pre-resolved candidate
//!   text (S:360, S:283, P1:184/186). No DNS is performed here: the caller
//!   supplies every candidate answer and the function verifies each one is
//!   publicly routable. There is no hostname dial input on the resulting
//!   plan, so a transport has nothing to re-resolve.
//! - [`admit_connection_step`]: one connect/checkout pair per window with
//!   terminal pre-content failure (B1:211/321 amendment A5).
//! - [`derive_capacity`]: whole-object bounds-table derivation with the
//!   provider/plan/50 clamp, 90% refusal and lane headroom
//!   (S:343-354, S:360, P1:184/226, P2:404).
//! - [`map_loader_error`]: fixed `{v:3,request_id,error:Code}` rendering
//!   with the 403/409/503/404 gate table (S:356).
//!
//! Fail-closed posture: every denial renders one fixed [`LoaderDenial`]
//! code; sizes, caps, counts, hosts and request IDs never echo into the
//! error. `DialPlan` deliberately implements no `Debug`: the vetted peer
//! is dial material and stays out of capture/debug/tail/trace surfaces.
//!
//! No vault, keys, sockets, database, clock, bindings or `unsafe` may ever
//! appear here; the slice containment fence enforces that spelling. The
//! address verdicts below mirror `nntune-ssrf-guard` exactly: this slice
//! admits no new dependencies, so the mainstream verdicts ride along
//! inline, and long-term classification stays single-owned by the shared
//! guard crate (a follow-up slice should add the path dependency and
//! delegate instead of extending this mirror).

use std::fmt::{Display, Formatter};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use crate::pool::Lane;

// ---- Bounds table (S:345-354) ----

/// Largest provider request envelope the loader admits whole.
pub const MAX_PROVIDER_REQUEST_BYTES: u64 = 8 * 1024;
/// Largest capability envelope admitted whole.
pub const MAX_CAPABILITY_BYTES: u64 = 4 * 1024;
/// Largest proof envelope admitted whole.
pub const MAX_PROOF_BYTES: u64 = 2 * 1024;
/// Largest nested reserve admitted whole.
pub const MAX_NESTED_RESERVE_BYTES: u64 = 8 * 1024;
/// Largest scope envelope admitted whole.
pub const MAX_SCOPE_ENVELOPE_BYTES: u64 = 256 * 1024;
/// Largest plan envelope admitted whole.
pub const MAX_PLAN_BYTES: u64 = 768 * 1024;
/// Largest metadata envelope admitted whole.
pub const MAX_METADATA_BYTES: u64 = 896 * 1024;
/// Largest manifest response admitted whole.
pub const MAX_MANIFEST_RESPONSE_BYTES: u64 = 1024 * 1024;
/// Largest manifest check admitted whole.
pub const MAX_MANIFEST_CHECK_BYTES: u64 = 1024;
/// Largest encoded article admitted whole.
pub const MAX_ENCODED_ARTICLE_BYTES: u64 = 2 * 1024 * 1024;
/// Largest decoded article admitted whole.
pub const MAX_DECODED_ARTICLE_BYTES: u64 = 1024 * 1024;
/// Largest block admitted whole.
pub const MAX_BLOCK_BYTES: u64 = 64 * 1024;
/// Largest extra buffer admitted whole.
pub const MAX_EXTRA_BUFFER_BYTES: u64 = 8 * 1024 * 1024;

/// Deepest scope nesting admitted.
pub const MAX_SCOPE_DEPTH: u32 = 8;
/// Most scope targets admitted.
pub const MAX_SCOPE_TARGETS: u32 = 500;
/// Most plan actions admitted.
pub const MAX_PLAN_ACTIONS: u32 = 2048;
/// Fewest plan actions admitted.
pub const MIN_PLAN_ACTIONS: u32 = 1;

/// Absolute ceiling on effective connections: `min(provider, plan, 50)`.
pub const MAX_EFFECTIVE_CONNECTIONS: i64 = 50;
/// Global usage refusal threshold in permille: 90% refuses new work.
pub const REFUSAL_USE_PERMILLE: u64 = 900;

/// Most retained JTIs tracked globally.
pub const MAX_RETAINED_JTIS: u64 = 100_000;
/// Most proof nonces tracked globally.
pub const MAX_PROOF_NONCES: u64 = 500_000;
/// Most audit outbox rows tracked globally.
pub const MAX_AUDIT_OUTBOX: u64 = 100_000;
/// Most global scopes tracked.
pub const MAX_GLOBAL_SCOPES: u64 = 100_000;
/// Most action rows tracked globally.
pub const MAX_ACTION_ROWS: u64 = 2_000_000;

/// Most non-terminal JTIs per owner.
pub const MAX_NONTERMINAL_JTIS_PER_OWNER: u32 = 8;
/// Most open scopes per owner.
pub const MAX_OPEN_SCOPES_PER_OWNER: u32 = 64;
/// Most issue attempts per owner per minute.
pub const MAX_ISSUE_ATTEMPTS_PER_OWNER_PER_MIN: u32 = 60;

// ---- Fixed denial codes ----

/// Closed loader denial: one fixed code, status and rendering per variant.
/// Inputs never echo: `Display` renders exactly [`LoaderDenial::code`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoaderDenial {
    /// Caller lacks entitlement (disabled plan, gated mapping).
    AuthorizationDenied,
    /// Provider storage exists but holds no configuration; mappable to
    /// 409 only after full authorization.
    ProviderNotConfigured,
    /// Stored endpoint or window cannot serve (unusable, saturated,
    /// quarantined, failed).
    ProviderUnavailable,
    /// Public or unrecognized route.
    NotFound,
}

impl LoaderDenial {
    /// Fixed wire code for this denial.
    pub fn code(&self) -> &'static str {
        match self {
            LoaderDenial::AuthorizationDenied => "authorization_denied",
            LoaderDenial::ProviderNotConfigured => "provider_not_configured",
            LoaderDenial::ProviderUnavailable => "provider_unavailable",
            LoaderDenial::NotFound => "not_found",
        }
    }

    /// Fixed HTTP status for this denial.
    pub fn status(&self) -> u16 {
        match self {
            LoaderDenial::AuthorizationDenied => 403,
            LoaderDenial::ProviderNotConfigured => 409,
            LoaderDenial::ProviderUnavailable => 503,
            LoaderDenial::NotFound => 404,
        }
    }
}

impl Display for LoaderDenial {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.code())
    }
}

// ---- Dial-plan vetting (S:360, S:283, P1:184/186) ----

/// NNTP ports the loader ever vets: plaintext 119 and implicit-TLS 563.
const NNTP_PORTS: [u16; 2] = [119, 563];

/// Vet-ALL dial-plan request: the candidate answers are inert pre-resolved
/// text supplied by the caller, never resolved here.
pub struct VetDialPlanRequest<'a> {
    /// Original hostname, carried byte-exact as the TLS server name.
    pub host: &'a str,
    /// Requested port: exactly 119 or 563.
    pub port: u16,
    /// True for implicit TLS, false for plaintext.
    pub tls: bool,
    /// Explicit stored acknowledgement lifting plaintext; required when
    /// `tls` is false.
    pub allow_insecure_plaintext: bool,
    /// Every candidate answer; ALL must parse and be publicly routable.
    pub candidates: &'a [&'a str],
}

/// Inert vetted destination: an exact peer plus the original TLS name. Not
/// a socket, not a dial permission, and deliberately not `Debug`.
pub struct DialPlan {
    peer: SocketAddr,
    server_name: String,
    tls: bool,
}

impl DialPlan {
    /// Exact vetted peer: a member of the vetted set with the requested
    /// port. No selection order is fixed.
    pub fn peer(&self) -> SocketAddr {
        self.peer
    }

    /// Original hostname byte-exact for TLS validation.
    pub fn tls_server_name(&self) -> &str {
        &self.server_name
    }

    /// Transport mode echoed from the request.
    pub fn tls(&self) -> bool {
        self.tls
    }
}

/// Static host shape (P1 lengths): 1..=253 ASCII bytes of dot-separated
/// non-empty labels, with no scheme, path, userinfo, port suffix or space.
/// No per-label charset rule is invented: labels only need to be non-empty,
/// and any `:` fails shape before literal parsing, so v6-literal hosts
/// deny exactly like the mirrored validator quirk.
fn host_shape_ok(host: &str) -> bool {
    let len = host.len();
    if len == 0 || len > 253 || !host.is_ascii() {
        return false;
    }
    if host
        .bytes()
        .any(|b| matches!(b, b':' | b'/' | b'?' | b'#' | b'@') || b.is_ascii_whitespace())
    {
        return false;
    }
    host.split('.').all(|label| !label.is_empty())
}

/// Mirror of the shared guard's v4 verdict: true when the octets must never
/// be dialed (loopback, private, link-local incl. the metadata endpoint,
/// carrier-grade NAT, this-network, multicast, benchmarking, IETF
/// assignments, reserved, broadcast, documentation).
fn v4_forbidden(octets: &[u8; 4]) -> bool {
    let v4 = Ipv4Addr::from(*octets);
    v4.is_loopback()
        || v4.is_multicast()
        || v4.is_link_local()
        || v4.is_private()
        || v4.is_broadcast()
        || v4.is_documentation()
        || octets[0] == 0
        || (octets[0] == 100 && (octets[1] & 0xC0) == 64)
        || (octets[0] == 192 && octets[1] == 0 && octets[2] == 0)
        || (octets[0] == 198 && (octets[1] == 18 || octets[1] == 19))
        || octets[0] >= 240
}

/// Mirror of the shared guard's address verdict: v6 admits only native
/// global unicast outside transition/special-use space.
fn ip_forbidden(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4_forbidden(&v4.octets()),
        IpAddr::V6(v6) => {
            let s = v6.segments();
            (s[0] & 0xE000) != 0x2000
                || s[0] == 0x2002
                || (s[0] == 0x2001 && s[1] < 0x0200)
                || (s[0] == 0x2001 && s[1] == 0x0DB8)
                || (s[0] == 0x3FFF && (s[1] & 0xF000) == 0)
        }
    }
}

/// Vet a dial plan: exact NNTP port, mirrored host shape, plaintext
/// acknowledgement, and ALL candidates parsed and publicly routable. Any
/// failure denies the whole plan as [`LoaderDenial::ProviderUnavailable`]:
/// the stored endpoint is unusable; the caller's own authorization is not
/// decided here.
pub fn vet_dial_plan(req: &VetDialPlanRequest) -> Result<DialPlan, LoaderDenial> {
    if !NNTP_PORTS.contains(&req.port) {
        return Err(LoaderDenial::ProviderUnavailable);
    }
    if !host_shape_ok(req.host) {
        return Err(LoaderDenial::ProviderUnavailable);
    }
    if !req.tls && !req.allow_insecure_plaintext {
        return Err(LoaderDenial::ProviderUnavailable);
    }
    if req.candidates.is_empty() {
        return Err(LoaderDenial::ProviderUnavailable);
    }
    let mut vetted: Option<IpAddr> = None;
    for raw in req.candidates.iter() {
        let ip: IpAddr = match raw.parse() {
            Ok(ip) => ip,
            Err(_) => return Err(LoaderDenial::ProviderUnavailable),
        };
        if ip_forbidden(&ip) {
            return Err(LoaderDenial::ProviderUnavailable);
        }
        if vetted.is_none() {
            vetted = Some(ip);
        }
    }
    match vetted {
        Some(ip) => Ok(DialPlan {
            peer: SocketAddr::new(ip, req.port),
            server_name: req.host.to_owned(),
            tls: req.tls,
        }),
        None => Err(LoaderDenial::ProviderUnavailable),
    }
}

// ---- One connect/checkout pair (B1:211/321 amendment A5) ----

/// Connection steps admitted at most once per window.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum DialStep {
    /// Open the transport.
    Connect,
    /// Check a connection out of the window.
    Checkout,
}

/// Per-step admission state within a window.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum StepState {
    /// Never presented.
    Unadmitted,
    /// Admitted, awaiting completion.
    Admitted,
    /// Finished.
    Completed,
    /// Terminally failed: the window admits nothing further.
    Failed,
}

/// Window carrying both step states; inert counts with no live handle.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct DialWindow {
    /// Connect-step state.
    pub connect: StepState,
    /// Checkout-step state.
    pub checkout: StepState,
}

/// Admit one connection step: fresh windows admit connect first; checkout
/// needs a completed connect, or a fresh window with `warm` (which spends
/// the skipped connect). Every step consumes exactly once, completed pairs
/// admit no second pair or reconnect, and any [`StepState::Failed`] state
/// terminally denies every further step. Denials are
/// [`LoaderDenial::ProviderUnavailable`]: the window cannot serve the step.
pub fn admit_connection_step(
    window: &DialWindow,
    step: DialStep,
    warm: bool,
) -> Result<(), LoaderDenial> {
    use StepState::{Completed, Failed, Unadmitted};
    if window.connect == Failed || window.checkout == Failed {
        return Err(LoaderDenial::ProviderUnavailable);
    }
    let ok = match step {
        DialStep::Connect => window.connect == Unadmitted && window.checkout == Unadmitted,
        DialStep::Checkout => {
            (window.connect == Completed && window.checkout == Unadmitted)
                || (warm && window.connect == Unadmitted && window.checkout == Unadmitted)
        }
    };
    if ok {
        Ok(())
    } else {
        Err(LoaderDenial::ProviderUnavailable)
    }
}

// ---- Capacity derivation (S:343-354, S:360, P1:184/226, P2:404) ----

/// Capacity-derivation request: inert sizes, caps, counts and policy bits.
pub struct CapacityRequest {
    /// Object size to admit whole, never truncated.
    pub object_bytes: u64,
    /// Bound the whole object must fit within, inclusive.
    pub bound_bytes: u64,
    /// Stored provider connection cap; must be positive.
    pub provider_cap: i64,
    /// Stored plan connection cap; zero/negative is disabled entitlement.
    pub plan_cap: i64,
    /// Priority lane requesting the slot.
    pub lane: Lane,
    /// Connections currently in flight.
    pub in_flight: u32,
    /// True when the pool quarantined this owner.
    pub quarantined: bool,
    /// Global records in use.
    pub global_used: u64,
    /// Global records bound.
    pub global_bound: u64,
    /// Owner records in use.
    pub owner_used: u32,
    /// Owner records bound.
    pub owner_bound: u32,
    /// Prior resolve attempts recorded for this identity.
    pub resolve_used: u32,
}

/// Whole-object capacity grant: the admitted size and the metered slot.
pub struct CapacityGrant {
    effective_max_connections: i64,
    object_bytes: u64,
    admitted_slot: u32,
}

impl CapacityGrant {
    /// Effective cap: `min(provider, plan, 50)` after validation.
    pub fn effective_max_connections(&self) -> i64 {
        self.effective_max_connections
    }

    /// Whole admitted size, never a truncated prefix.
    pub fn object_bytes(&self) -> u64 {
        self.object_bytes
    }

    /// Metering input: the in-flight count after this grant.
    pub fn admitted_slot(&self) -> u32 {
        self.admitted_slot
    }
}

/// Derive capacity: one generic inclusive (actual, bound) mechanism backs
/// every byte/shape ceiling (fit admits, bound+1 denies, never a partial
/// grant). Caps validate before any clamp — a zero/disabled plan denies as
/// [`LoaderDenial::AuthorizationDenied`] (entitlement) and never routes
/// through the pool clamp helper; every other violation denies as
/// [`LoaderDenial::ProviderUnavailable`]. Provider oversize above 50 denies
/// unless the plan is already at the ceiling, where the clamp absorbs it.
/// Global 90%-of-bound refuses new work (overflow-safe; saturated counters
/// deny); owner ceilings deny at the bound; any recorded prior resolve
/// denies the retry; quarantine denies before slot accounting; lanes keep
/// the pool's exact reserve (Playback 0, others 1); at-cap denies as
/// backpressure with no queue or partial slot.
pub fn derive_capacity(req: &CapacityRequest) -> Result<CapacityGrant, LoaderDenial> {
    use LoaderDenial::{AuthorizationDenied, ProviderUnavailable};
    if req.plan_cap <= 0 {
        return Err(AuthorizationDenied);
    }
    if req.provider_cap <= 0 {
        return Err(ProviderUnavailable);
    }
    if req.provider_cap > MAX_EFFECTIVE_CONNECTIONS && req.plan_cap < MAX_EFFECTIVE_CONNECTIONS {
        return Err(ProviderUnavailable);
    }
    if req.object_bytes > req.bound_bytes {
        return Err(ProviderUnavailable);
    }
    let over_refusal = u128::from(req.global_used) * 1000
        >= u128::from(req.global_bound) * u128::from(REFUSAL_USE_PERMILLE);
    if over_refusal {
        return Err(ProviderUnavailable);
    }
    if req.owner_used >= req.owner_bound {
        return Err(ProviderUnavailable);
    }
    if req.resolve_used != 0 {
        return Err(ProviderUnavailable);
    }
    if req.quarantined {
        return Err(ProviderUnavailable);
    }
    let effective = req
        .provider_cap
        .min(req.plan_cap)
        .min(MAX_EFFECTIVE_CONNECTIONS);
    // Pool reserve mirror (pool keeps the live rule): playback takes the
    // last slot while prefetch/ingest leave one free for playback.
    let reserve = match req.lane {
        Lane::Playback => 0,
        Lane::Prefetch | Lane::Ingest => 1,
    };
    // `effective` is 1..=50 here: both caps validated positive above and
    // the clamp bounds the top, so the narrowing cast cannot truncate.
    let headroom = (effective as u32).saturating_sub(reserve);
    if req.in_flight >= headroom {
        return Err(ProviderUnavailable);
    }
    Ok(CapacityGrant {
        effective_max_connections: effective,
        object_bytes: req.object_bytes,
        admitted_slot: req.in_flight.saturating_add(1),
    })
}

// ---- Fixed error mapping (S:356) ----

/// Mapped loader error: fixed status plus the exact three-key body.
pub struct MappedLoaderError {
    status: u16,
    body: String,
}

impl MappedLoaderError {
    /// Fixed HTTP status for the mapped denial.
    pub fn status(&self) -> u16 {
        self.status
    }

    /// Exact wire body: `{"v":3,"request_id":"<id>","error":"<code>"}`.
    pub fn body(&self) -> &str {
        &self.body
    }
}

/// True for the echoable request-ID shape: 36 bytes of lowercase hex with
/// dashes at 8/13/18/23, the version nibble `4` and a UUIDv4 variant nibble
/// (`8`/`9`/`a`/`b`). Anything else — including uppercase, wrong versions,
/// wrong variants and overlong inputs — takes the caller-supplied fresh ID
/// instead, never echoing caller bytes.
fn request_id_ok(id: &str) -> bool {
    let bytes = id.as_bytes();
    if bytes.len() != 36 {
        return false;
    }
    if bytes[8] != b'-' || bytes[13] != b'-' || bytes[18] != b'-' || bytes[23] != b'-' {
        return false;
    }
    if bytes[14] != b'4' {
        return false;
    }
    if !matches!(bytes[19], b'8' | b'9' | b'a' | b'b') {
        return false;
    }
    bytes
        .iter()
        .enumerate()
        .all(|(i, b)| matches!(i, 8 | 13 | 18 | 23) || matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Map a denial to its fixed wire shape. The 409 (and the 503) render only
/// after full authorization — otherwise 403, with no existence disclosure;
/// unknown routes map to 404 regardless. A valid lowercase v4 request ID
/// echoes byte-exact; anything else is replaced by the caller-supplied
/// authority UUID verbatim.
pub fn map_loader_error(
    denial: LoaderDenial,
    request_id: &str,
    fresh_id: &str,
    authorized: bool,
) -> MappedLoaderError {
    let mapped = match (denial, authorized) {
        (LoaderDenial::NotFound, _) => LoaderDenial::NotFound,
        (LoaderDenial::AuthorizationDenied, _) => LoaderDenial::AuthorizationDenied,
        (LoaderDenial::ProviderNotConfigured, true) => LoaderDenial::ProviderNotConfigured,
        (LoaderDenial::ProviderUnavailable, true) => LoaderDenial::ProviderUnavailable,
        _ => LoaderDenial::AuthorizationDenied,
    };
    let id = if request_id_ok(request_id) {
        request_id
    } else {
        fresh_id
    };
    MappedLoaderError {
        status: mapped.status(),
        body: format!(
            "{{\"v\":3,\"request_id\":\"{id}\",\"error\":\"{}\"}}",
            mapped.code()
        ),
    }
}
