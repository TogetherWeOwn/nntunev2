//! Test-only handoff: DNS resolution must finish before the independent socket
//! budget begins. A permanently pending AsyncDns must not retain a vault request
//! indefinitely. Removing the handler's resolution deadline, retaining the losing
//! resolver future, or dialing before all-address validation breaks these tests.
//!
//! The 20-second watchdog is a test liveness ceiling, NOT the production DNS
//! budget and NOT a replacement for the existing 5s dial / 15s socket budgets.
//! It never supplies a timeout result to the handler. On success the resolver's
//! drop witness is checked while the completed handler future is still retained;
//! dropping the test's future cannot manufacture cancellation evidence.
//!
//! These native tests exercise real handlers with synthetic Store/AsyncDns/Dialer
//! boundaries. They do not prove that dropping a Worker fetch aborts its headers
//! or body stream: the Builder must separately demonstrate actual adapter abort
//! wiring for both A and AAAA in an offline Worker runtime fixture. No source-text
//! assertion substitutes for that runtime evidence. No live DNS or sockets here.

use std::cell::{Cell, RefCell};
use std::future::Future;
use std::net::IpAddr;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use nntune_api::{auth, handlers, session, vault};

const NOW: u64 = 1_700_000_000;
const ORIGIN: &str = "https://passkey.example.com";
const SECRET: &[u8] = b"synthetic-dns-deadline-session-secret";
const HOST: &str = "news.example.com";
const INDEXER: &str = "index.example.com";
const WATCHDOG: Duration = Duration::from_secs(20);
const PUBLIC_V4: &str = "93.184.216.34";
const PUBLIC_V6: &str = "2606:4700:4700::1111";

// This executor polls the production future until it returns or the independent
// watchdog expires. It does not drop the caller-owned future on expiry.
fn wait<F: Future>(mut future: Pin<&mut F>) -> Option<F::Output> {
    let mut cx = Context::from_waker(std::task::Waker::noop());
    let end = Instant::now() + WATCHDOG;
    loop {
        if let Poll::Ready(value) = future.as_mut().poll(&mut cx) {
            return Some(value);
        }
        if Instant::now() >= end {
            return None;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn complete<F: Future>(future: F) -> F::Output {
    wait(Box::pin(future).as_mut()).expect("fixture operation exceeded watchdog")
}

#[derive(Default)]
struct Store {
    session: RefCell<Option<session::SessionRecord>>,
    vault: RefCell<Option<handlers::VaultRow>>,
    writes: Cell<usize>,
    leases: Cell<usize>,
    audits: RefCell<Vec<vault::AuditEvent>>,
}

// Unrelated auth operations fail loudly if routing accidentally reaches them.
impl handlers::Store for Store {
    async fn user_by_email(
        &self,
        _: &str,
    ) -> Result<Option<handlers::UserRecord>, handlers::StoreError> {
        Err(handlers::StoreError)
    }
    async fn insert_user(&self, _: &handlers::UserRecord) -> Result<(), handlers::StoreError> {
        Err(handlers::StoreError)
    }
    async fn insert_challenge(
        &self,
        _: &auth::PasskeyChallenge,
        _: Option<&str>,
        _: &str,
    ) -> Result<(), handlers::StoreError> {
        Err(handlers::StoreError)
    }
    async fn challenge_by_id(
        &self,
        _: &str,
    ) -> Result<Option<handlers::ChallengeRow>, handlers::StoreError> {
        Err(handlers::StoreError)
    }
    async fn insert_credential(
        &self,
        _: &auth::PasskeyCredential,
    ) -> Result<(), handlers::StoreError> {
        Err(handlers::StoreError)
    }
    async fn credential_by_id(
        &self,
        _: &str,
    ) -> Result<Option<auth::PasskeyCredential>, handlers::StoreError> {
        Err(handlers::StoreError)
    }
    async fn credentials_for_user(
        &self,
        _: &str,
    ) -> Result<Vec<auth::PasskeyCredential>, handlers::StoreError> {
        Err(handlers::StoreError)
    }
    async fn advance_sign_count(
        &self,
        _: &str,
        _: u32,
        _: u32,
    ) -> Result<bool, handlers::StoreError> {
        Err(handlers::StoreError)
    }
    async fn insert_magic(&self, _: &auth::MagicLinkRecord) -> Result<(), handlers::StoreError> {
        Err(handlers::StoreError)
    }
    async fn magic_by_hash(
        &self,
        _: &str,
    ) -> Result<Option<auth::MagicLinkRecord>, handlers::StoreError> {
        Err(handlers::StoreError)
    }
    async fn invite_by_hash(
        &self,
        _: &str,
    ) -> Result<Option<auth::InviteRecord>, handlers::StoreError> {
        Err(handlers::StoreError)
    }
    async fn insert_session(&self, _: &session::SessionRecord) -> Result<(), handlers::StoreError> {
        Err(handlers::StoreError)
    }
    async fn session_by_id(
        &self,
        id: &str,
    ) -> Result<Option<session::SessionRecord>, handlers::StoreError> {
        Ok(self
            .session
            .borrow()
            .as_ref()
            .filter(|row| row.id == id)
            .cloned())
    }
    async fn rotate_session_row(
        &self,
        _: &str,
        _: u32,
        _: u32,
        _: u64,
    ) -> Result<bool, handlers::StoreError> {
        Err(handlers::StoreError)
    }
    async fn revoke_session(&self, _: &str) -> Result<(), handlers::StoreError> {
        Err(handlers::StoreError)
    }
    async fn revoke_all_sessions(&self, _: &str) -> Result<(), handlers::StoreError> {
        Err(handlers::StoreError)
    }
    async fn vault_by_user(
        &self,
        _: &str,
    ) -> Result<Option<handlers::VaultRow>, handlers::StoreError> {
        Ok(self.vault.borrow().clone())
    }
    async fn upsert_vault(
        &self,
        _: &str,
        row: &handlers::VaultRow,
    ) -> Result<(), handlers::StoreError> {
        self.writes.set(self.writes.get() + 1);
        self.vault.replace(Some(row.clone()));
        Ok(())
    }
    async fn delete_vault(&self, _: &str) -> Result<(), handlers::StoreError> {
        Err(handlers::StoreError)
    }
    async fn insert_audit(&self, event: &vault::AuditEvent) -> Result<(), handlers::StoreError> {
        self.audits.borrow_mut().push(vault::AuditEvent {
            user_id: event.user_id.clone(),
            action: event.action.clone(),
            ok: event.ok,
            source_ip: event.source_ip.clone(),
            at: event.at,
        });
        Ok(())
    }
    async fn reserve_rate_limit(
        &self,
        _: &str,
        _: &str,
        _: u64,
        _: u64,
        _: u64,
    ) -> Result<bool, handlers::StoreError> {
        Ok(true)
    }
    async fn prune_expired_challenges(&self, _: u64) -> Result<(), handlers::StoreError> {
        Ok(())
    }
    async fn acquire_test_connection_lease(
        &self,
        _: &str,
        _: &str,
        _: u64,
        _: u32,
    ) -> Result<bool, handlers::StoreError> {
        self.leases.set(self.leases.get() + 1);
        Ok(true)
    }
    async fn release_test_connection_lease(
        &self,
        _: &str,
        _: &str,
    ) -> Result<(), handlers::StoreError> {
        self.leases.set(
            self.leases
                .get()
                .checked_sub(1)
                .expect("release an acquired lease"),
        );
        Ok(())
    }
}

impl auth::SingleUseStore for Store {
    async fn claim(&self, _: auth::SingleUseClaim<'_>) -> Result<u64, auth::AuthError> {
        panic!("vault route must not consume auth claims")
    }
}

struct Mailer;
impl handlers::Mailer for Mailer {
    async fn send_magic_link(&self, _: &str, _: &str) -> Result<(), handlers::StoreError> {
        panic!("vault route must not send email")
    }
}

#[derive(Default)]
struct Dialer {
    calls: RefCell<Vec<(Vec<IpAddr>, u16, bool)>>,
}
impl handlers::Dialer for Dialer {
    async fn probe(&self, target: &vault::ValidatedTarget, tls: bool) -> bool {
        // The dialer receives no caller-supplied port: the only port it can
        // record is the validated target's own allowlisted port.
        self.calls
            .borrow_mut()
            .push((target.addresses().to_vec(), target.port(), tls));
        true
    }
}

struct Dns {
    stalled_host: Cell<Option<&'static str>>,
    answers: RefCell<Vec<IpAddr>>,
    queries: RefCell<Vec<String>>,
    started: Cell<usize>,
    dropped: Cell<usize>,
}
impl Default for Dns {
    fn default() -> Self {
        Self {
            stalled_host: Cell::new(None),
            answers: RefCell::new(vec![PUBLIC_V4.parse().unwrap(), PUBLIC_V6.parse().unwrap()]),
            queries: RefCell::new(Vec::new()),
            started: Cell::new(0),
            dropped: Cell::new(0),
        }
    }
}
struct PendingResolution<'a> {
    dns: &'a Dns,
    polled: bool,
}
impl Future for PendingResolution<'_> {
    type Output = Vec<IpAddr>;
    fn poll(mut self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Self::Output> {
        if !self.polled {
            self.dns.started.set(self.dns.started.get() + 1);
            self.polled = true;
        }
        Poll::Pending
    }
}
impl Drop for PendingResolution<'_> {
    fn drop(&mut self) {
        if self.polled {
            self.dns.dropped.set(self.dns.dropped.get() + 1);
        }
    }
}
impl handlers::AsyncDns for Dns {
    fn resolve<'a>(&'a self, host: &'a str) -> Pin<Box<dyn Future<Output = Vec<IpAddr>> + 'a>> {
        self.queries.borrow_mut().push(host.to_string());
        if self.stalled_host.get() == Some(host) {
            return Box::pin(PendingResolution {
                dns: self,
                polled: false,
            });
        }
        // Mirror the resolver boundary's literal result without any real lookup.
        let answers = host
            .parse::<IpAddr>()
            .map(|ip| vec![ip])
            .unwrap_or_else(|_| self.answers.borrow().clone());
        Box::pin(std::future::ready(answers))
    }
}

struct Harness {
    cfg: handlers::Config,
    store: Store,
    dns: Dns,
    dialer: Dialer,
    cookie: String,
}
impl Harness {
    fn new() -> Self {
        let token = session::issue_session(SECRET, "dns-user", NOW, 3600).unwrap();
        let claims = session::verify_session(SECRET, &token, NOW).unwrap();
        let store = Store::default();
        store.session.replace(Some(session::SessionRecord {
            id: claims.session_id,
            user_id: "dns-user".into(),
            created_at: NOW,
            expires_at: NOW + 3600,
            rotation_counter: 0,
            revoked: false,
        }));
        Self {
            cfg: handlers::Config::new(
                "passkey.example.com",
                ORIGIN,
                SECRET.to_vec(),
                [7; 32],
                1,
                3600,
                false,
                false,
                None,
            )
            .unwrap(),
            store,
            dns: Dns::default(),
            dialer: Dialer::default(),
            cookie: format!("{}={token}", session::COOKIE_NAME),
        }
    }
    fn ctx(&self) -> handlers::Ctx<'_, Store, Mailer, Dialer, Dns> {
        handlers::Ctx {
            cfg: &self.cfg,
            store: &self.store,
            mailer: &Mailer,
            dialer: &self.dialer,
            dns: &self.dns,
            now: NOW,
            source_ip: "198.51.100.9".into(),
        }
    }
    fn request(&self, method: &str, path: &str, body: serde_json::Value) -> handlers::Request {
        handlers::Request {
            method: method.into(),
            path: path.into(),
            cookie: Some(self.cookie.clone()),
            origin: Some(ORIGIN.into()),
            authorization: None,
            body: if body.is_null() {
                vec![]
            } else {
                serde_json::to_vec(&body).unwrap()
            },
        }
    }
    fn save(&self) {
        let ctx = self.ctx();
        let req = self.request("PUT", "/v1/vault", payload(HOST, None));
        let response = complete(handlers::handle(&ctx, &req));
        assert_eq!(response.status, 200, "fixture save: {}", response.body);
    }
    fn assert_expiry(&self, request: handlers::Request, probe: bool) {
        let writes = self.store.writes.get();
        let ctx = self.ctx();
        let mut handler = Box::pin(handlers::handle(&ctx, &request));
        let response = wait(handler.as_mut());
        assert_eq!(
            self.dns.started.get(),
            1,
            "request must reach the pending resolver"
        );
        assert!(
            self.dialer.calls.borrow().is_empty(),
            "DNS expiry must perform zero socket dials"
        );
        assert_eq!(
            self.store.writes.get(),
            writes,
            "DNS expiry must not save a vault"
        );
        assert!(response.is_some(),
            "DNS resolution exceeded the 20s watchdog: handler still pending; started={}, dropped={}, dials={}",
            self.dns.started.get(), self.dns.dropped.get(), self.dialer.calls.borrow().len());
        let response = response.unwrap();
        assert_eq!(self.dns.dropped.get(), 1, "expiry must drop the pending resolver BEFORE returning, not when the test drops the handler");
        assert_eq!(
            self.store.leases.get(),
            0,
            "DNS expiry must release probe concurrency leases"
        );
        if probe {
            assert_eq!(response.status, 200);
            assert_eq!(
                response.body, r#"{"ok":false}"#,
                "expired DNS must retain the generic public result"
            );
        } else {
            assert_eq!(response.status, 400);
            assert_eq!(response.body, r#"{"error":"vault unavailable"}"#);
        }
        assert!(response.set_cookie.is_none());
        // Deliberately keep `handler` alive through the witness assertions.
        drop(handler);
    }
}
fn payload(host: &str, indexer: Option<&str>) -> serde_json::Value {
    serde_json::json!({
        "provider": {"host": host, "port": 563, "tls": true,
            "username": "synthetic-user", "password": "synthetic-pass", "max_connections": 4},
        "indexer": indexer.map(|host| serde_json::json!({"url": format!("https://{host}/api"), "api_key": "synthetic-key"}))
    })
}

#[test]
fn pending_provider_resolution_finishes_save_and_cancels_without_dialing() {
    let h = Harness::new();
    h.dns.stalled_host.set(Some(HOST));
    h.assert_expiry(h.request("PUT", "/v1/vault", payload(HOST, None)), false);
}

#[test]
fn pending_indexer_resolution_finishes_save_without_partial_write_or_dial() {
    let h = Harness::new();
    h.dns.stalled_host.set(Some(INDEXER));
    h.assert_expiry(
        h.request("PUT", "/v1/vault", payload(HOST, Some(INDEXER))),
        false,
    );
}

#[test]
fn pending_saved_provider_resolution_finishes_probe_and_releases_lease() {
    let h = Harness::new();
    h.save();
    h.dns.stalled_host.set(Some(HOST));
    h.assert_expiry(
        h.request("POST", "/v1/vault/test-connection", serde_json::Value::Null),
        true,
    );
}

#[test]
fn pending_supplied_provider_resolution_finishes_probe_without_dialing() {
    let h = Harness::new();
    h.dns.stalled_host.set(Some(HOST));
    h.assert_expiry(
        h.request("POST", "/v1/vault/test-connection", payload(HOST, None)),
        true,
    );
    assert!(
        h.store.vault.borrow().is_none(),
        "candidate probe must not persist credentials"
    );
}

#[test]
fn caller_cancellation_drops_the_pending_resolver_without_dialing() {
    let h = Harness::new();
    h.dns.stalled_host.set(Some(HOST));
    let ctx = h.ctx();
    let req = h.request("PUT", "/v1/vault", payload(HOST, None));
    let mut handler = Box::pin(handlers::handle(&ctx, &req));
    let mut cx = Context::from_waker(std::task::Waker::noop());
    assert!(handler.as_mut().poll(&mut cx).is_pending());
    assert_eq!(h.dns.started.get(), 1);
    assert_eq!(h.dns.dropped.get(), 0);
    drop(handler);
    assert_eq!(
        h.dns.dropped.get(),
        1,
        "witness must observe actual future cancellation"
    );
    assert!(h.dialer.calls.borrow().is_empty());
    assert_eq!(h.store.writes.get(), 0);
}

#[test]
fn fast_dual_family_resolution_dials_only_validated_pinned_addresses() {
    let h = Harness::new();
    h.save();
    let ctx = h.ctx();
    let req = h.request("POST", "/v1/vault/test-connection", serde_json::Value::Null);
    let response = complete(handlers::handle(&ctx, &req));
    assert_eq!(response.status, 200, "{}", response.body);
    let body: serde_json::Value = serde_json::from_str(&response.body).unwrap();
    assert_eq!(body["ok"], true);
    assert_eq!(
        *h.dialer.calls.borrow(),
        vec![(
            vec![
                PUBLIC_V4.parse::<IpAddr>().unwrap(),
                PUBLIC_V6.parse().unwrap()
            ],
            563,
            true
        )]
    );
    assert_eq!(h.store.leases.get(), 0);
}

#[test]
fn mixed_forbidden_answers_reject_the_whole_target_even_after_dial_cap() {
    for forbidden in ["127.0.0.1", "::1"] {
        for position in [0, 4] {
            let h = Harness::new();
            h.save();
            let mut answers = [
                "93.184.216.34",
                "93.184.216.35",
                "93.184.216.36",
                "93.184.216.37",
                "93.184.216.38",
            ]
            .map(|ip| ip.parse::<IpAddr>().unwrap())
            .to_vec();
            answers.insert(position, forbidden.parse().unwrap());
            h.dns.answers.replace(answers);
            let ctx = h.ctx();
            let req = h.request("POST", "/v1/vault/test-connection", serde_json::Value::Null);
            let response = complete(handlers::handle(&ctx, &req));
            assert_eq!(response.status, 200);
            let body: serde_json::Value = serde_json::from_str(&response.body).unwrap();
            assert_eq!(
                body["ok"], false,
                "forbidden answer {forbidden} at {position}"
            );
            assert!(
                h.dialer.calls.borrow().is_empty(),
                "must validate ALL answers before capping/dialing"
            );
            assert_eq!(h.store.leases.get(), 0);
        }
    }
}

#[test]
fn fast_resolution_keeps_the_four_pinned_address_dial_cap() {
    let h = Harness::new();
    h.save();
    h.dns.answers.replace(
        [
            "93.184.216.34",
            "93.184.216.35",
            "93.184.216.36",
            "93.184.216.37",
            "93.184.216.38",
            "93.184.216.39",
        ]
        .map(|ip| ip.parse::<IpAddr>().unwrap())
        .to_vec(),
    );
    let ctx = h.ctx();
    let req = h.request("POST", "/v1/vault/test-connection", serde_json::Value::Null);
    let response = complete(handlers::handle(&ctx, &req));
    assert_eq!(response.body, r#"{"ok":true}"#);
    assert_eq!(
        *h.dialer.calls.borrow(),
        vec![(
            vec![
                "93.184.216.34".parse::<IpAddr>().unwrap(),
                "93.184.216.35".parse().unwrap(),
                "93.184.216.36".parse().unwrap(),
                "93.184.216.37".parse().unwrap(),
            ],
            563,
            true
        )]
    );
}

#[test]
fn forbidden_literal_provider_never_dials() {
    for literal in ["127.0.0.1", "::1"] {
        let h = Harness::new();
        let ctx = h.ctx();
        let req = h.request("POST", "/v1/vault/test-connection", payload(literal, None));
        let response = complete(handlers::handle(&ctx, &req));
        assert_eq!(response.status, 200);
        assert_eq!(response.body, r#"{"ok":false}"#);
        assert!(h.dialer.calls.borrow().is_empty());
        assert!(h.store.vault.borrow().is_none());
    }
}

#[test]
fn public_ipv4_literal_provider_remains_usable() {
    // Raw IPv6 provider-host syntax is rejected by the baseline policy; this
    // deadline change must not redesign it. Dual-family DNS is tested above.
    let literal = PUBLIC_V4;
    let h = Harness::new();
    let ctx = h.ctx();
    let save = h.request("PUT", "/v1/vault", payload(literal, None));
    let response = complete(handlers::handle(&ctx, &save));
    assert_eq!(response.status, 200, "literal save: {}", response.body);
    let probe = h.request("POST", "/v1/vault/test-connection", serde_json::Value::Null);
    let response = complete(handlers::handle(&ctx, &probe));
    assert_eq!(response.status, 200);
    let body: serde_json::Value = serde_json::from_str(&response.body).unwrap();
    assert_eq!(body["ok"], true);
    assert_eq!(
        *h.dialer.calls.borrow(),
        vec![(vec![literal.parse::<IpAddr>().unwrap()], 563, true)]
    );
}
