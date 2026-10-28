//! Independent accepted ADR 0002/0006 API-key-only contract, NOT the inert 0023
//! scalar-pepper bridge. Synthetic keys/rows only; this is no runtime approval.
//!
//! Required native-testable PRODUCTION seam in `subsonic_auth`:
//! - SubsonicOwnerRow { user_id, email: String, deleting_at,
//!   subsonic_disabled_at: Option<i64> }. Missing owner is NOT all-enabled.
//! - SubsonicClientRow: Clone + redacted Debug { client_id, user_id,
//!   subsonic_username, api_key_hmac, auth_mode, scope: String,
//!   pepper_version: u32, revoked_at: Option<i64>, owner: Option<SubsonicOwnerRow> }.
//! - SubsonicClientLookup async methods: clients_by_candidates(&[(u32,String)])
//!   -> Result<Vec<Row>, StoreError>; cas_pepper(&Row,u32,&str)
//!   -> Result<Option<Row>, StoreError> (None = zero rows); mark_last_used(&Row)
//!   -> Result<bool, StoreError>; unrevoked_for_pepper(u32) -> Result<u64,StoreError>.
//!   CAS/last-used must fence the actual owner in the SAME transaction; this
//!   fake alone cannot prove SQL locking. See subsonic_credential_sql_acceptance.
//! - SubsonicAuthAttempt { ip: String }; SubsonicAuthLimiter async methods
//!   check_general(&Attempt), check_auth_attempt(&Attempt), check_credential(&str)
//!   -> Result<bool, StoreError>. These are synthetic decision doubles, NOT a
//!   claim that a process-local limiter is suitable for deployed enforcement.
//! - prepare_subsonic_auth(store: Option<&S>, keyring_json: Option<&str>,
//!   limiter: Option<&L>, attempt: &Attempt, credentials: &Credentials) returns
//!   PreparedSubsonicAuth: AuthStore + redacted Debug. fault() returns
//!   Option<SubsonicAuthFault> (Copy + Eq + Debug): Keyring, Configuration,
//!   Store, Limiter. Every operational failure still has nondisclosing wire 44.
//! - subsonic_pepper_retirement_allowed(&S,u32) -> Result<bool,StoreError> is a
//!   metadata-only guard, NOT permission or an API to rotate/retire a pepper.
//!
//! The JSON version-to-key map is named `keys` in these synthetic seam fixtures;
//! ADRs prescribe the structure, not a raw-key alphabet. Duplicate JSON members
//! must be rejected, not silently overwritten by a generic map deserializer.
//! Tests call real preparation/authentication, replacing only external storage
//! and limiter decisions. Missing new seam is intentional compile-RED, never an
//! excuse for an unused scalar wrapper. Worker must consume this SAME helper.

#[path = "../../../crates/nntune-subsonic/tests/support/client_auth_library.rs"]
mod client_auth_library;

use client_auth_library::GuardedLibrary;
use nntune_api::handlers::StoreError;
use nntune_api::subsonic_auth::{
    prepare_subsonic_auth, subsonic_pepper_retirement_allowed, SubsonicAuthAttempt,
    SubsonicAuthFault, SubsonicAuthLimiter, SubsonicClientLookup, SubsonicClientRow,
    SubsonicOwnerRow,
};
use nntune_subsonic::{AuthStore, Credentials, HttpResponse, Identity, Params, SubsonicApp};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::future::Future;
use std::rc::Rc;
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};

// Public synthetic vectors, independently calculated with Python hmac/sha256.
const V1_PEPPER: &str = "synthetic-subsonic-index-pepper-v1!";
const V2_PEPPER: &str = "synthetic-subsonic-index-pepper-v2!";
const CURRENT: &str = r#"{"current_version":2,"keys":{"2":"synthetic-subsonic-index-pepper-v2!"}}"#;
const TRANSITION: &str = r#"{"current_version":2,"previous_version":1,"keys":{"1":"synthetic-subsonic-index-pepper-v1!","2":"synthetic-subsonic-index-pepper-v2!"}}"#;
const ALPHA_KEY: &str = "synthetic-client-alpha-0123456789abcdef";
const BETA_KEY: &str = "synthetic-client-beta-fedcba9876543210";
const ALPHA_V1: &str = "524f316146ebc41db98bf8e3881dd7038659624f1061d24b30d8ab1a1d576a05";
const ALPHA_V2: &str = "5c886144a4a0aa546256e6c76c05f0c37ebf37bb53bd7b0d56f08b7bf486f57e";
const BETA_V2: &str = "cf2222a424e5493c02593df5ee876b7c23bb8d259f1885cefb60786189e24893";
const ALPHA_ID: &str = "00000000-0000-4000-8000-000000000001";
const BETA_ID: &str = "00000000-0000-4000-8000-000000000002";
const ALPHA_USER: &str = "synthetic-persisted-user-alpha";
const BETA_USER: &str = "synthetic-persisted-user-beta";

type Pair = (u32, String);
type Events = Rc<RefCell<Vec<Event>>>;

#[derive(Clone, Debug, PartialEq, Eq)]
enum Event {
    General(String),
    Ip(String),
    Lookup(Vec<Pair>),
    Credential(String),
    Cas {
        id: String,
        owner: String,
        old: Pair,
        new: Pair,
    },
    LastUsed {
        id: String,
        owner: String,
        pair: Pair,
    },
    Inventory(u32),
}

fn identity(user: &str) -> Identity {
    Identity {
        username: user.into(),
        email: if user == ALPHA_USER {
            "alpha@example.test"
        } else {
            "beta@example.test"
        }
        .into(),
        is_admin: false,
    }
}

fn row(id: &str, user: &str, version: u32, hmac: &str) -> SubsonicClientRow {
    SubsonicClientRow {
        client_id: id.into(),
        user_id: user.into(),
        subsonic_username: format!("credential-{id}"),
        api_key_hmac: hmac.into(),
        pepper_version: version,
        auth_mode: "apikey".into(),
        scope: "subsonic".into(),
        revoked_at: None,
        owner: Some(SubsonicOwnerRow {
            user_id: user.into(),
            email: identity(user).email,
            deleting_at: None,
            subsonic_disabled_at: None,
        }),
    }
}

fn alpha() -> SubsonicClientRow {
    row(ALPHA_ID, ALPHA_USER, 2, ALPHA_V2)
}
fn old_alpha() -> SubsonicClientRow {
    row(ALPHA_ID, ALPHA_USER, 1, ALPHA_V1)
}
fn beta() -> SubsonicClientRow {
    row(BETA_ID, BETA_USER, 2, BETA_V2)
}

enum LookupReply {
    Rows(Vec<SubsonicClientRow>),
    Error,
}
enum CasReply {
    Update,
    Zero,
    Error,
    Returned(Box<SubsonicClientRow>),
}

struct SyntheticStore {
    rows: RefCell<Vec<SubsonicClientRow>>,
    replies: RefCell<VecDeque<LookupReply>>,
    cas: RefCell<CasReply>,
    mark: RefCell<Result<bool, ()>>,
    inventory_error: RefCell<bool>,
    successful_marks: RefCell<Vec<String>>,
    events: Events,
}

impl SubsonicClientLookup for SyntheticStore {
    async fn clients_by_candidates(
        &self,
        pairs: &[Pair],
    ) -> Result<Vec<SubsonicClientRow>, StoreError> {
        self.events.borrow_mut().push(Event::Lookup(pairs.to_vec()));
        if let Some(reply) = self.replies.borrow_mut().pop_front() {
            return match reply {
                LookupReply::Rows(rows) => Ok(rows),
                LookupReply::Error => Err(StoreError),
            };
        }
        Ok(self
            .rows
            .borrow()
            .iter()
            .filter(|r| pairs.contains(&(r.pepper_version, r.api_key_hmac.clone())))
            .cloned()
            .collect())
    }

    async fn cas_pepper(
        &self,
        expected: &SubsonicClientRow,
        version: u32,
        hmac: &str,
    ) -> Result<Option<SubsonicClientRow>, StoreError> {
        self.events.borrow_mut().push(Event::Cas {
            id: expected.client_id.clone(),
            owner: expected.user_id.clone(),
            old: (expected.pepper_version, expected.api_key_hmac.clone()),
            new: (version, hmac.into()),
        });
        match &*self.cas.borrow() {
            CasReply::Error => return Err(StoreError),
            CasReply::Zero => return Ok(None),
            CasReply::Returned(row) => return Ok(Some(row.as_ref().clone())),
            CasReply::Update => {}
        }
        let mut rows = self.rows.borrow_mut();
        let Some(row) = rows.iter_mut().find(|r| {
            r.client_id == expected.client_id
                && r.user_id == expected.user_id
                && r.pepper_version == expected.pepper_version
                && r.api_key_hmac == expected.api_key_hmac
                && r.revoked_at.is_none()
        }) else {
            return Ok(None);
        };
        row.pepper_version = version;
        row.api_key_hmac = hmac.into();
        Ok(Some(row.clone()))
    }

    async fn mark_last_used(&self, row: &SubsonicClientRow) -> Result<bool, StoreError> {
        self.events.borrow_mut().push(Event::LastUsed {
            id: row.client_id.clone(),
            owner: row.user_id.clone(),
            pair: (row.pepper_version, row.api_key_hmac.clone()),
        });
        match &*self.mark.borrow() {
            Err(()) => Err(StoreError),
            Ok(false) => Ok(false),
            Ok(true) => {
                self.successful_marks
                    .borrow_mut()
                    .push(row.client_id.clone());
                Ok(true)
            }
        }
    }

    async fn unrevoked_for_pepper(&self, version: u32) -> Result<u64, StoreError> {
        self.events.borrow_mut().push(Event::Inventory(version));
        if *self.inventory_error.borrow() {
            return Err(StoreError);
        }
        Ok(self
            .rows
            .borrow()
            .iter()
            .filter(|r| r.pepper_version == version && r.revoked_at.is_none())
            .count() as u64)
    }
}

#[derive(Clone, Copy)]
enum Limit {
    Allow,
    Deny,
    Error,
}

struct SyntheticLimiter {
    general: RefCell<Limit>,
    ip: RefCell<Limit>,
    credential: RefCell<Limit>,
    events: Events,
}

fn limit_result(limit: Limit) -> Result<bool, StoreError> {
    match limit {
        Limit::Allow => Ok(true),
        Limit::Deny => Ok(false),
        Limit::Error => Err(StoreError),
    }
}

impl SubsonicAuthLimiter for SyntheticLimiter {
    async fn check_general(&self, attempt: &SubsonicAuthAttempt) -> Result<bool, StoreError> {
        self.events
            .borrow_mut()
            .push(Event::General(attempt.ip.clone()));
        limit_result(*self.general.borrow())
    }
    async fn check_auth_attempt(&self, attempt: &SubsonicAuthAttempt) -> Result<bool, StoreError> {
        self.events.borrow_mut().push(Event::Ip(attempt.ip.clone()));
        limit_result(*self.ip.borrow())
    }
    async fn check_credential(&self, id: &str) -> Result<bool, StoreError> {
        self.events.borrow_mut().push(Event::Credential(id.into()));
        limit_result(*self.credential.borrow())
    }
}

struct Scenario {
    store: SyntheticStore,
    limiter: SyntheticLimiter,
    events: Events,
}
impl Scenario {
    fn new(rows: Vec<SubsonicClientRow>) -> Self {
        let events = Rc::new(RefCell::new(Vec::new()));
        Self {
            store: SyntheticStore {
                rows: RefCell::new(rows),
                replies: RefCell::new(VecDeque::new()),
                cas: RefCell::new(CasReply::Update),
                mark: RefCell::new(Ok(true)),
                inventory_error: RefCell::new(false),
                successful_marks: RefCell::new(Vec::new()),
                events: Rc::clone(&events),
            },
            limiter: SyntheticLimiter {
                general: RefCell::new(Limit::Allow),
                ip: RefCell::new(Limit::Allow),
                credential: RefCell::new(Limit::Allow),
                events: Rc::clone(&events),
            },
            events,
        }
    }
    fn request(&self, keyring: Option<&str>, query: &str) -> Outcome {
        request(Some(&self.store), keyring, Some(&self.limiter), query)
    }
    fn lookup_pairs(&self) -> Vec<Vec<Pair>> {
        self.events
            .borrow()
            .iter()
            .filter_map(|e| {
                if let Event::Lookup(pairs) = e {
                    Some(pairs.clone())
                } else {
                    None
                }
            })
            .collect()
    }
    fn cas_count(&self) -> usize {
        self.events
            .borrow()
            .iter()
            .filter(|e| matches!(e, Event::Cas { .. }))
            .count()
    }
    fn assert_no_writes(&self) {
        assert!(
            !self
                .events
                .borrow()
                .iter()
                .any(|e| matches!(e, Event::Cas { .. } | Event::LastUsed { .. })),
            "denied request wrote credential state"
        );
        assert!(self.store.successful_marks.borrow().is_empty());
    }
}

fn finish<F: Future>(future: F) -> F::Output {
    let mut future = std::pin::pin!(future);
    let mut context = Context::from_waker(Waker::noop());
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if let Poll::Ready(value) = future.as_mut().poll(&mut context) {
            return value;
        }
        assert!(
            Instant::now() < deadline,
            "synthetic authentication did not complete"
        );
        std::thread::yield_now();
    }
}

fn credentials(query: &str) -> Credentials {
    let params = Params::parse(query);
    Credentials::from_params(|key| params.get(key).map(str::to_owned))
}
fn attempt() -> SubsonicAuthAttempt {
    SubsonicAuthAttempt {
        ip: "192.0.2.10".into(),
    }
}

struct Outcome {
    response: HttpResponse,
    calls: Vec<Identity>,
    fault: Option<SubsonicAuthFault>,
    debug: String,
}
fn request(
    store: Option<&SyntheticStore>,
    keyring: Option<&str>,
    limiter: Option<&SyntheticLimiter>,
    query: &str,
) -> Outcome {
    let auth = finish(prepare_subsonic_auth(
        store,
        keyring,
        limiter,
        &attempt(),
        &credentials(query),
    ));
    let fault = auth.fault();
    let debug = format!("{auth:?}\n{auth:#?}\n{:#?}", Some(&auth));
    let (library, calls) = GuardedLibrary::new(auth);
    let response = SubsonicApp::new(library).handle(
        "GET",
        "/rest/getPlaylists",
        &format!("f=json&{query}"),
        "",
    );
    let calls = calls.borrow().clone();
    Outcome {
        response,
        calls,
        fault,
        debug,
    }
}
fn key_query() -> String {
    format!("apiKey={ALPHA_KEY}")
}
fn assert_denied(out: &Outcome, code: i64, message: &str) {
    assert_eq!(out.response.status, 200);
    assert_eq!(out.response.content_type, "application/json");
    let document: serde_json::Value = serde_json::from_slice(&out.response.body).unwrap();
    assert_eq!(document["subsonic-response"]["status"], "failed");
    assert_eq!(document["subsonic-response"]["error"]["code"], code);
    assert_eq!(document["subsonic-response"]["error"]["message"], message);
    assert!(
        out.calls.is_empty(),
        "denied auth reached protected Library"
    );
}
fn assert_invalid(out: &Outcome) {
    assert_denied(out, 44, "Invalid API key.");
}
fn assert_owner(out: &Outcome, user: &str) {
    let document: serde_json::Value = serde_json::from_slice(&out.response.body).unwrap();
    assert_eq!(out.response.status, 200);
    assert_eq!(document["subsonic-response"]["status"], "ok");
    assert_eq!(out.calls, [identity(user)]);
    assert_eq!(out.fault, None);
}
fn assert_private(output: &str) {
    for canary in [
        ALPHA_KEY,
        BETA_KEY,
        ALPHA_V1,
        ALPHA_V2,
        BETA_V2,
        V1_PEPPER,
        V2_PEPPER,
        "synthetic-password-canary",
        "synthetic-token-canary",
        "synthetic-salt-canary",
        "synthetic-u-canary",
    ] {
        assert!(
            !output.contains(canary),
            "credential disclosed in diagnostic/envelope"
        );
    }
}

#[test]
fn current_only_candidates_are_version_bound_and_resolve_exact_persisted_nonadmin_users() {
    let s = Scenario::new(vec![alpha(), beta()]);
    assert_owner(&s.request(Some(CURRENT), &key_query()), ALPHA_USER);
    assert_owner(
        &s.request(Some(CURRENT), &format!("apiKey={BETA_KEY}")),
        BETA_USER,
    );
    assert_eq!(
        s.lookup_pairs(),
        [vec![(2, ALPHA_V2.into())], vec![(2, BETA_V2.into())]]
    );
    assert_eq!(s.cas_count(), 0);
    assert_eq!(*s.store.successful_marks.borrow(), [ALPHA_ID, BETA_ID]);
}

#[test]
fn transition_computes_only_current_and_previous_pairs_even_on_a_current_hit() {
    let s = Scenario::new(vec![alpha()]);
    assert_owner(&s.request(Some(TRANSITION), &key_query()), ALPHA_USER);
    assert_eq!(
        s.lookup_pairs(),
        [vec![(2, ALPHA_V2.into()), (1, ALPHA_V1.into())]]
    );
    assert_eq!(s.cas_count(), 0);
}

#[test]
fn scalar_missing_malformed_duplicate_or_extra_active_keyrings_are_deny_only() {
    for config in [
        None,
        Some(""),
        Some(V2_PEPPER),
        Some("not-json"),
        Some("null"),
        Some("{}"),
        Some(r#""synthetic-subsonic-index-pepper-v2!""#),
        Some("[]"),
        Some(r#"{"current_version":2147483648,"keys":{"2147483648":"synthetic"}}"#),
        Some(r#"{"current_version":2,"previous_version":0,"keys":{"0":"a","2":"b"}}"#),
        Some(r#"{"current_version":2,"previous_version":-1,"keys":{"-1":"a","2":"b"}}"#),
        Some(r#"{"current_version":2,"keys":"synthetic"}"#),
        Some(r#"{"current_version":2,"keys":{}}"#),
        Some(r#"{"current_version":2,"keys":{"2":""}}"#),
        Some(r#"{"current_version":0,"keys":{"0":"synthetic"}}"#),
        Some(r#"{"current_version":-1,"keys":{"-1":"synthetic"}}"#),
        Some(r#"{"current_version":2.5,"keys":{"2":"synthetic"}}"#),
        Some(r#"{"current_version":"2","keys":{"2":"synthetic"}}"#),
        Some(r#"{"current_version":2,"previous_version":2,"keys":{"2":"synthetic"}}"#),
        Some(r#"{"current_version":2,"previous_version":1,"keys":{"2":"synthetic"}}"#),
        Some(r#"{"current_version":2,"previous_version":1,"keys":{"1":"","2":"synthetic"}}"#),
        Some(r#"{"current_version":2,"keys":{"2":null}}"#),
        Some(r#"{"current_version":2,"keys":{"2":23}}"#),
        Some(r#"{"current_version":2,"keys":{"2":"synthetic","3":"unreferenced"}}"#),
        Some(r#"{"current_version":2,"previous_version":1,"keys":{"1":"a","2":"b","3":"c"}}"#),
        Some(r#"{"current_version":2,"current_version":1,"keys":{"1":"a","2":"b"}}"#),
        Some(r#"{"current_version":2,"keys":{"2":"a","2":"b"}}"#),
        Some(r#"{"current_version":2,"keys":{"02":"a"}}"#),
    ] {
        let s = Scenario::new(vec![alpha()]);
        let out = s.request(config, &key_query());
        assert_invalid(&out);
        assert_eq!(out.fault, Some(SubsonicAuthFault::Keyring));
        assert!(s.lookup_pairs().is_empty());
        s.assert_no_writes();
        assert_private(&out.debug);
    }
}

#[test]
fn opaque_key_bytes_are_not_trimmed_casefolded_or_restricted_to_a_new_alphabet() {
    for query in [
        format!("apiKey=%20{ALPHA_KEY}"),
        format!("apiKey={ALPHA_KEY}%20"),
        format!("apiKey={}", ALPHA_KEY.to_ascii_uppercase()),
    ] {
        let s = Scenario::new(vec![alpha()]);
        assert_invalid(&s.request(Some(CURRENT), &query));
        s.assert_no_writes();
    }
    let s = Scenario::new(vec![row(
        ALPHA_ID,
        ALPHA_USER,
        2,
        "81008994431e3c3a34d4c246b2d5d7615613a9854cc469d6dfb5a82053fe8da0",
    )]);
    assert_owner(
        &s.request(
            Some(CURRENT),
            "apiKey=synthetic%20key%20%2B%2F%3F%20not%20an%20invented%20format",
        ),
        ALPHA_USER,
    );
}

#[test]
fn wrong_pair_malformed_digest_or_distinct_matches_are_not_trusted() {
    let mut wrong_version = alpha();
    wrong_version.pepper_version = 1;
    let mut uppercase = alpha();
    uppercase.api_key_hmac = ALPHA_V2.to_ascii_uppercase();
    let mut short = alpha();
    short.api_key_hmac = "5c88".into();
    let mut zero = alpha();
    zero.pepper_version = 0;
    let mut same_digest_other_id = alpha();
    same_digest_other_id.client_id = BETA_ID.into();
    let old_other_owner = row(BETA_ID, BETA_USER, 1, ALPHA_V1);
    for rows in [
        vec![wrong_version],
        vec![uppercase],
        vec![short],
        vec![zero],
        vec![beta()],
        vec![alpha(), old_alpha()],
        vec![alpha(), same_digest_other_id],
        vec![alpha(), old_other_owner],
    ] {
        let s = Scenario::new(Vec::new());
        s.store
            .replies
            .borrow_mut()
            .push_back(LookupReply::Rows(rows));
        assert_invalid(&s.request(Some(TRANSITION), &key_query()));
        s.assert_no_writes();
    }
}

#[test]
fn missing_wrong_or_ineligible_owner_mode_scope_and_revocation_all_deny() {
    let mut variants = Vec::new();
    let mut missing = alpha();
    missing.owner = None;
    variants.push(missing);
    let mut wrong = alpha();
    wrong.owner.as_mut().unwrap().user_id = BETA_USER.into();
    variants.push(wrong);
    let mut no_id = alpha();
    no_id.user_id.clear();
    variants.push(no_id);
    for marker in [1_i64, 4_000_000_000] {
        let mut deleting = alpha();
        deleting.owner.as_mut().unwrap().deleting_at = Some(marker);
        variants.push(deleting);
        let mut disabled = alpha();
        disabled.owner.as_mut().unwrap().subsonic_disabled_at = Some(marker);
        variants.push(disabled);
        let mut revoked = alpha();
        revoked.revoked_at = Some(marker);
        variants.push(revoked);
    }
    for mode in ["token", "both", "", "APIKEY"] {
        let mut r = alpha();
        r.auth_mode = mode.into();
        variants.push(r);
    }
    for scope in ["native", "provider", "ingest", "", "SUBSONIC"] {
        let mut r = alpha();
        r.scope = scope.into();
        variants.push(r);
    }
    let unknown = Scenario::new(Vec::new())
        .request(Some(CURRENT), &key_query())
        .response;
    for r in variants {
        let s = Scenario::new(vec![r]);
        let out = s.request(Some(CURRENT), &key_query());
        assert_invalid(&out);
        assert_eq!(out.response, unknown);
        s.assert_no_writes();
    }
}

#[test]
fn revoking_one_client_does_not_revoke_its_sibling_or_cache_an_owner() {
    let mut sibling = beta();
    sibling.user_id = ALPHA_USER.into();
    sibling.owner = alpha().owner;
    let s = Scenario::new(vec![alpha(), sibling]);
    assert_owner(&s.request(Some(CURRENT), &key_query()), ALPHA_USER);
    s.store.rows.borrow_mut()[0].revoked_at = Some(1);
    assert_invalid(&s.request(Some(CURRENT), &key_query()));
    assert_owner(
        &s.request(Some(CURRENT), &format!("apiKey={BETA_KEY}")),
        ALPHA_USER,
    );
    assert_eq!(*s.store.successful_marks.borrow(), [ALPHA_ID, BETA_ID]);
}

#[test]
fn owner_lifecycle_and_store_failures_are_rechecked_after_prior_success() {
    for change in ["missing", "deleting", "disabled", "store"] {
        let s = Scenario::new(vec![alpha()]);
        assert_owner(&s.request(Some(CURRENT), &key_query()), ALPHA_USER);
        match change {
            "missing" => s.store.rows.borrow_mut()[0].owner = None,
            "deleting" => {
                s.store.rows.borrow_mut()[0]
                    .owner
                    .as_mut()
                    .unwrap()
                    .deleting_at = Some(4_000_000_000)
            }
            "disabled" => {
                s.store.rows.borrow_mut()[0]
                    .owner
                    .as_mut()
                    .unwrap()
                    .subsonic_disabled_at = Some(1)
            }
            _ => s.store.replies.borrow_mut().push_back(LookupReply::Error),
        }
        assert_invalid(&s.request(Some(CURRENT), &key_query()));
        assert_eq!(*s.store.successful_marks.borrow(), [ALPHA_ID]);
    }
}

#[test]
fn old_version_success_cas_binds_id_owner_old_pair_and_current_pair_before_last_used() {
    let s = Scenario::new(vec![old_alpha()]);
    assert_owner(&s.request(Some(TRANSITION), &key_query()), ALPHA_USER);
    let events = s.events.borrow();
    let cas = Event::Cas {
        id: ALPHA_ID.into(),
        owner: ALPHA_USER.into(),
        old: (1, ALPHA_V1.into()),
        new: (2, ALPHA_V2.into()),
    };
    let mark = Event::LastUsed {
        id: ALPHA_ID.into(),
        owner: ALPHA_USER.into(),
        pair: (2, ALPHA_V2.into()),
    };
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, Event::Cas { .. }))
            .collect::<Vec<_>>(),
        [&cas]
    );
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, Event::LastUsed { .. }))
            .collect::<Vec<_>>(),
        [&mark]
    );
    assert!(
        events.iter().position(|e| e == &cas).unwrap()
            < events.iter().position(|e| e == &mark).unwrap()
    );
}

#[test]
fn cas_errors_including_uniqueness_conflicts_deny_without_retry_or_last_used() {
    // The persistence boundary reports a constraint error through StoreError,
    // never as a successful zero-row concurrent-winner receipt.
    let s = Scenario::new(vec![old_alpha()]);
    *s.store.cas.borrow_mut() = CasReply::Error;
    let out = s.request(Some(TRANSITION), &key_query());
    assert_invalid(&out);
    assert_eq!(out.fault, Some(SubsonicAuthFault::Store));
    assert_eq!(s.cas_count(), 1);
    assert_eq!(s.lookup_pairs().len(), 1);
    assert!(s.store.successful_marks.borrow().is_empty());
}

fn lost_cas_scenario(retry: LookupReply) -> Scenario {
    let s = Scenario::new(vec![old_alpha()]);
    *s.store.cas.borrow_mut() = CasReply::Zero;
    s.store
        .replies
        .borrow_mut()
        .extend([LookupReply::Rows(vec![old_alpha()]), retry]);
    s
}

#[test]
fn zero_row_cas_allows_one_fresh_current_only_lookup_of_the_same_credential_and_owner() {
    let s = lost_cas_scenario(LookupReply::Rows(vec![alpha()]));
    assert_owner(&s.request(Some(TRANSITION), &key_query()), ALPHA_USER);
    assert_eq!(
        s.lookup_pairs(),
        [
            vec![(2, ALPHA_V2.into()), (1, ALPHA_V1.into())],
            vec![(2, ALPHA_V2.into())]
        ]
    );
    assert_eq!(s.cas_count(), 1);
    assert_eq!(*s.store.successful_marks.borrow(), [ALPHA_ID]);
}

#[test]
fn cas_result_or_retry_uses_fresh_owner_projection_not_the_earlier_identity() {
    for zero_rows in [true, false] {
        let mut fresh = alpha();
        fresh.owner.as_mut().unwrap().email = "fresh-alpha@example.test".into();
        let s = if zero_rows {
            lost_cas_scenario(LookupReply::Rows(vec![fresh.clone()]))
        } else {
            Scenario::new(vec![old_alpha()])
        };
        if !zero_rows {
            *s.store.cas.borrow_mut() = CasReply::Returned(Box::new(fresh));
        }
        let out = s.request(Some(TRANSITION), &key_query());
        let document: serde_json::Value = serde_json::from_slice(&out.response.body).unwrap();
        assert_eq!(document["subsonic-response"]["status"], "ok");
        assert_eq!(
            out.calls,
            [Identity {
                username: ALPHA_USER.into(),
                email: "fresh-alpha@example.test".into(),
                is_admin: false
            }]
        );
        assert_eq!(s.cas_count(), 1);
    }
}

#[test]
fn current_only_configuration_cannot_trust_an_old_pair_returned_by_storage() {
    let s = Scenario::new(Vec::new());
    s.store
        .replies
        .borrow_mut()
        .push_back(LookupReply::Rows(vec![old_alpha()]));
    assert_invalid(&s.request(Some(CURRENT), &key_query()));
    s.assert_no_writes();
}

#[test]
fn zero_row_cas_does_not_reuse_earlier_identity_on_missing_error_or_old_retry() {
    for reply in [
        LookupReply::Rows(Vec::new()),
        LookupReply::Error,
        LookupReply::Rows(vec![old_alpha()]),
    ] {
        let s = lost_cas_scenario(reply);
        assert_invalid(&s.request(Some(TRANSITION), &key_query()));
        assert_eq!(s.lookup_pairs().len(), 2);
        assert_eq!(s.cas_count(), 1, "no second CAS or retry loop");
        assert!(s.store.successful_marks.borrow().is_empty());
    }
}

#[test]
fn retry_and_successful_cas_rows_are_fully_revalidated_including_raced_owner_state() {
    let mut variants = Vec::new();
    let mut different_id = alpha();
    different_id.client_id = BETA_ID.into();
    variants.push(different_id);
    let mut different_owner = alpha();
    different_owner.user_id = BETA_USER.into();
    different_owner.owner = beta().owner;
    variants.push(different_owner);
    let mut wrong_projection = alpha();
    wrong_projection.owner = beta().owner;
    variants.push(wrong_projection);
    let mut missing = alpha();
    missing.owner = None;
    variants.push(missing);
    let mut revoked = alpha();
    revoked.revoked_at = Some(1);
    variants.push(revoked);
    let mut deleting = alpha();
    deleting.owner.as_mut().unwrap().deleting_at = Some(4_000_000_000);
    variants.push(deleting);
    let mut disabled = alpha();
    disabled.owner.as_mut().unwrap().subsonic_disabled_at = Some(1);
    variants.push(disabled);
    let mut mode = alpha();
    mode.auth_mode = "token".into();
    variants.push(mode);
    let mut scope = alpha();
    scope.scope = "native".into();
    variants.push(scope);
    let mut digest = alpha();
    digest.api_key_hmac = BETA_V2.into();
    variants.push(digest);
    let mut version = alpha();
    version.pepper_version = 1;
    variants.push(version);
    variants.push(old_alpha());
    for candidate in variants {
        for zero_rows in [true, false] {
            let s = if zero_rows {
                lost_cas_scenario(LookupReply::Rows(vec![candidate.clone()]))
            } else {
                Scenario::new(vec![old_alpha()])
            };
            if !zero_rows {
                *s.store.cas.borrow_mut() = CasReply::Returned(Box::new(candidate.clone()));
            }
            assert_invalid(&s.request(Some(TRANSITION), &key_query()));
            assert_eq!(s.cas_count(), 1);
            assert_eq!(s.lookup_pairs().len(), if zero_rows { 2 } else { 1 });
            assert!(s.store.successful_marks.borrow().is_empty());
        }
    }
    let s = lost_cas_scenario(LookupReply::Rows(vec![alpha(), beta()]));
    assert_invalid(&s.request(Some(TRANSITION), &key_query()));
    assert_eq!(s.cas_count(), 1);
    assert_eq!(s.lookup_pairs().len(), 2);
}

#[test]
fn last_used_is_a_separate_success_only_owner_fenced_write_whose_failure_denies() {
    for verdict in [Ok(false), Err(())] {
        let s = Scenario::new(vec![alpha()]);
        *s.store.mark.borrow_mut() = verdict;
        assert_invalid(&s.request(Some(CURRENT), &key_query()));
        assert_eq!(s.cas_count(), 0);
        assert!(s.store.successful_marks.borrow().is_empty());
    }
}

#[test]
fn dormant_unrevoked_old_credentials_block_retirement_even_for_ineligible_owners() {
    for owner_state in ["active", "deleting", "disabled", "missing"] {
        let mut dormant = old_alpha();
        match owner_state {
            "deleting" => dormant.owner.as_mut().unwrap().deleting_at = Some(1),
            "disabled" => dormant.owner.as_mut().unwrap().subsonic_disabled_at = Some(1),
            "missing" => dormant.owner = None,
            _ => {}
        }
        let s = Scenario::new(vec![dormant]);
        assert!(!finish(subsonic_pepper_retirement_allowed(&s.store, 1)).unwrap());
        s.assert_no_writes();
    }
    let mut historical = old_alpha();
    historical.revoked_at = Some(1);
    let s = Scenario::new(vec![historical, alpha()]);
    assert!(finish(subsonic_pepper_retirement_allowed(&s.store, 1)).unwrap());
    *s.store.inventory_error.borrow_mut() = true;
    assert!(finish(subsonic_pepper_retirement_allowed(&s.store, 1)).is_err());
    s.assert_no_writes();
}

#[test]
fn shared_general_and_ip_limits_run_before_lookup_including_unknown_keys() {
    for key in [ALPHA_KEY, "synthetic-unknown-key"] {
        let s = Scenario::new(vec![alpha()]);
        let out = s.request(Some(CURRENT), &format!("apiKey={key}"));
        if key == ALPHA_KEY {
            assert_owner(&out, ALPHA_USER);
        } else {
            assert_invalid(&out);
        }
        let events = s.events.borrow();
        let lookup = events
            .iter()
            .position(|e| matches!(e, Event::Lookup(_)))
            .unwrap();
        assert!(
            events
                .iter()
                .position(|e| e == &Event::General("192.0.2.10".into()))
                .unwrap()
                < lookup
        );
        assert!(
            events
                .iter()
                .position(|e| e == &Event::Ip("192.0.2.10".into()))
                .unwrap()
                < lookup
        );
        if key != ALPHA_KEY {
            assert!(!events.iter().any(|e| matches!(e, Event::Credential(_))));
        }
    }
}

#[test]
fn prelookup_limiter_denials_and_failures_never_query_or_write_credentials() {
    for (general, ip) in [
        (Limit::Deny, Limit::Allow),
        (Limit::Error, Limit::Allow),
        (Limit::Allow, Limit::Deny),
        (Limit::Allow, Limit::Error),
    ] {
        for key in [ALPHA_KEY, "synthetic-unknown-key"] {
            let s = Scenario::new(vec![alpha()]);
            *s.limiter.general.borrow_mut() = general;
            *s.limiter.ip.borrow_mut() = ip;
            let out = s.request(Some(CURRENT), &format!("apiKey={key}"));
            assert_invalid(&out);
            if matches!(general, Limit::Error) || matches!(ip, Limit::Error) {
                assert_eq!(out.fault, Some(SubsonicAuthFault::Limiter));
            }
            assert!(s.lookup_pairs().is_empty());
            s.assert_no_writes();
        }
    }
}

#[test]
fn per_credential_limit_runs_after_resolution_but_before_dispatch_or_success_writes() {
    for limit in [Limit::Allow, Limit::Deny, Limit::Error] {
        for row in [alpha(), old_alpha()] {
            let s = Scenario::new(vec![row]);
            *s.limiter.credential.borrow_mut() = limit;
            let out = s.request(Some(TRANSITION), &key_query());
            if matches!(limit, Limit::Allow) {
                assert_owner(&out, ALPHA_USER);
            } else {
                assert_invalid(&out);
                s.assert_no_writes();
            }
            if matches!(limit, Limit::Error) {
                assert_eq!(out.fault, Some(SubsonicAuthFault::Limiter));
            }
            let events = s.events.borrow();
            let lookup = events
                .iter()
                .position(|e| matches!(e, Event::Lookup(_)))
                .unwrap();
            let credential = events
                .iter()
                .position(|e| e == &Event::Credential(ALPHA_ID.into()))
                .unwrap();
            assert!(lookup < credential);
            if matches!(limit, Limit::Allow) {
                let mark = events
                    .iter()
                    .position(|e| matches!(e, Event::LastUsed { .. }))
                    .unwrap();
                assert!(credential < mark);
            }
        }
    }
}

#[test]
fn absent_required_store_or_limiter_is_configuration_fault_not_a_noop() {
    let s = Scenario::new(vec![alpha()]);
    for (store, limiter) in [(None, Some(&s.limiter)), (Some(&s.store), None)] {
        let out = request(store, Some(CURRENT), limiter, &key_query());
        assert_invalid(&out);
        assert_eq!(out.fault, Some(SubsonicAuthFault::Configuration));
    }
    assert!(s.lookup_pairs().is_empty());
    s.assert_no_writes();
}

#[test]
fn store_keyring_configuration_and_limiter_faults_are_distinct_but_wire_identical() {
    let s = Scenario::new(vec![alpha()]);
    let keyring = s.request(None, &key_query());
    let config = request(Some(&s.store), Some(CURRENT), None, &key_query());
    s.store.replies.borrow_mut().push_back(LookupReply::Error);
    let store = s.request(Some(CURRENT), &key_query());
    *s.limiter.general.borrow_mut() = Limit::Error;
    let limiter = s.request(Some(CURRENT), &key_query());
    for (out, fault) in [
        (&keyring, SubsonicAuthFault::Keyring),
        (&config, SubsonicAuthFault::Configuration),
        (&store, SubsonicAuthFault::Store),
        (&limiter, SubsonicAuthFault::Limiter),
    ] {
        assert_invalid(out);
        assert_eq!(out.fault, Some(fault));
        assert_eq!(out.response, keyring.response);
        assert_private(&out.debug);
        assert_private(&format!("{:?}\n{:#?}", out.fault, out.fault));
    }
}

#[test]
fn protocol_refusals_precede_any_persisted_lookup_or_legacy_fallback() {
    for (query, code, message) in [
        (
            format!("apiKey={ALPHA_KEY}&u="),
            43,
            "Conflicting authentication parameters.",
        ),
        (
            "apiKey=&u=".into(),
            43,
            "Conflicting authentication parameters.",
        ),
        (
            format!("apiKey={ALPHA_KEY}&p=plain"),
            43,
            "Conflicting authentication parameters.",
        ),
        (
            format!("apiKey={ALPHA_KEY}&t=token"),
            43,
            "Conflicting authentication parameters.",
        ),
        (
            format!("apiKey={ALPHA_KEY}&s=salt"),
            43,
            "Conflicting authentication parameters.",
        ),
        (
            format!("u=credential-{ALPHA_ID}&p={ALPHA_KEY}"),
            42,
            "Provided authentication mechanism not supported.",
        ),
        (
            "p=enc:zz".into(),
            42,
            "Provided authentication mechanism not supported.",
        ),
        (
            "u=owner&p=enc:73796e746865746963".into(),
            42,
            "Provided authentication mechanism not supported.",
        ),
        (
            "u=owner&t=token&s=salt".into(),
            40,
            "Wrong username or password.",
        ),
        (
            "u=owner&t=token&s=".into(),
            10,
            "Required parameter is missing.",
        ),
        ("apiKey=".into(), 40, "Wrong username or password."),
        ("".into(), 40, "Wrong username or password."),
    ] {
        let s = Scenario::new(vec![alpha()]);
        assert_denied(&s.request(Some(CURRENT), &query), code, message);
        assert!(s.lookup_pairs().is_empty());
        s.assert_no_writes();
    }
    let s = Scenario::new(vec![alpha()]);
    assert_owner(
        &s.request(Some(CURRENT), &format!("apiKey={ALPHA_KEY}&p=&t=&s=")),
        ALPHA_USER,
    );
}

#[test]
fn prepared_result_is_bound_to_exact_presented_key_not_another_request_or_owner() {
    let s = Scenario::new(vec![alpha(), beta()]);
    let auth = finish(prepare_subsonic_auth(
        Some(&s.store),
        Some(CURRENT),
        Some(&s.limiter),
        &attempt(),
        &credentials(&key_query()),
    ));
    assert_eq!(auth.verify_api_key(ALPHA_KEY), Some(identity(ALPHA_USER)));
    for key in [BETA_KEY, "", "test-key-1", "test-key-2"] {
        assert_eq!(auth.verify_api_key(key), None);
    }
    assert_eq!(auth.verify_password(ALPHA_USER, ALPHA_KEY), None);
    assert_eq!(auth.verify_token(ALPHA_USER, "token", "salt"), None);
    let (library, calls) = GuardedLibrary::new(auth);
    let response = SubsonicApp::new(library).handle(
        "POST",
        "/rest/getPlaylists.view",
        "f=json",
        &format!("apiKey={BETA_KEY}"),
    );
    let calls = calls.borrow().clone();
    assert_invalid(&Outcome {
        response,
        calls,
        fault: None,
        debug: String::new(),
    });
}

#[test]
fn valid_web_session_or_native_key_is_not_a_subsonic_fallback() {
    use nntune_api::session::{
        issue_session, verify_session, verify_stored_session, SessionKeys, SessionRecord,
    };
    let secret = b"synthetic-session-signing-secret-32-bytes";
    let now = 1_800_000_000;
    let token = issue_session(secret, ALPHA_USER, now, 300).unwrap();
    let claims = verify_session(secret, &token, now).unwrap();
    let record = SessionRecord {
        id: claims.session_id.clone(),
        user_id: claims.user_id.clone(),
        created_at: claims.created_at,
        expires_at: claims.expires_at,
        rotation_counter: claims.rotation_counter,
        revoked: false,
    };
    assert!(verify_stored_session(&SessionKeys::single(secret), &token, now, &record).is_ok());
    let s = Scenario::new(vec![alpha()]);
    assert_invalid(&s.request(Some(CURRENT), &format!("apiKey={token}")));
    assert_denied(
        &s.request(Some(CURRENT), &format!("u=owner&p={token}")),
        42,
        "Provided authentication mechanism not supported.",
    );
    // A published/native-looking string is not a credential just because of
    // its prefix. The only authority here is a matching scoped persisted row.
    for key in ["ntk_synthetic-native-only", "test-key-1", "test-key-2"] {
        assert_invalid(&s.request(Some(CURRENT), &format!("apiKey={key}")));
    }
    s.assert_no_writes();
}

#[test]
fn rows_prepared_results_and_nested_error_envelopes_never_disclose_material() {
    let s = Scenario::new(vec![alpha()]);
    let out = s.request(Some(CURRENT), &key_query());
    assert_owner(&out, ALPHA_USER);
    assert_private(&out.debug);
    for r in [alpha(), old_alpha(), beta()] {
        assert_private(&format!("{r:?}\n{r:#?}\n{:#?}", Some(&r)));
    }
    for f in ["json", "xml", "jsonp&callback=safeCallback"] {
        let params = format!("f={f}&apiKey={ALPHA_KEY}&u=synthetic-u-canary&p=synthetic-password-canary&t=synthetic-token-canary&s=synthetic-salt-canary");
        let auth = finish(prepare_subsonic_auth(
            Some(&s.store),
            Some(TRANSITION),
            Some(&s.limiter),
            &attempt(),
            &credentials(&params),
        ));
        assert_private(&format!("{auth:?}\n{auth:#?}"));
        let (library, calls) = GuardedLibrary::new(auth);
        let response = SubsonicApp::new(library).handle("GET", "/rest/getPlaylists", &params, "");
        assert_private(std::str::from_utf8(&response.body).unwrap());
        assert_private(&format!("{response:?}\n{response:#?}"));
        assert!(calls.borrow().is_empty());
    }
}
