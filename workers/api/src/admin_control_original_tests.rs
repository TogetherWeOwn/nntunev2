use super::*;
use crate::admin_original::{OriginalError, OriginalMetadata, OriginalRepository};

struct NoOriginal;

impl OriginalRepository for NoOriginal {
    async fn metadata(&mut self, _: &str) -> Result<Vec<OriginalMetadata>, OriginalError> {
        panic!("denied caller reached original metadata")
    }

    async fn envelope(
        &mut self,
        _: &OriginalMetadata,
    ) -> Result<Option<admin_ingest::IngestOriginalEnvelope>, OriginalError> {
        panic!("denied caller reached original envelope")
    }

    async fn confirm_current(&mut self, _: &OriginalMetadata) -> Result<bool, OriginalError> {
        panic!("denied caller reached original confirmation")
    }
}

fn request(cookie: Option<String>) -> Request {
    Request {
        method: "POST".into(),
        // Internal preparation is deliberately not a public control route.
        path: "/v1/admin/ingest".into(),
        cookie,
        origin: Some(ORIGIN.into()),
        authorization: None,
        stripe_signature: None,
        body: Vec::new(),
    }
}

fn denied(ctx: &Ctx<'_, FakeStore, FakeMailer, FakeDialer, FakeDns>, req: &Request) -> Response {
    // Even a malformed job identity cannot replace the non-enumerating guard.
    match block_on(read_admin_control_original(ctx, req, "", &mut NoOriginal)) {
        Err(response) => response,
        Ok(_) => panic!("unauthorized original opened"),
    }
}

#[test]
fn control_original_authentication_denies_before_repository_access() {
    let h = harness();
    seed_user(&h, "member1", "member@example.com");
    let member = magic_login(&h, "member@example.com");
    let admin = seed_admin(&h, "admin1", "admin@example.com");
    let unknown = not_found();
    let mut cases = vec![
        request(None),
        request(Some(format!("{}=invalid", session::COOKIE_NAME))),
        request(Some(member)),
    ];
    let mut bearer = request(None);
    bearer.authorization = Some("Bearer ntk_synthetic".into());
    cases.push(bearer.clone());
    bearer.cookie = Some(admin.clone());
    cases.push(bearer);
    let mut missing_origin = request(Some(admin.clone()));
    missing_origin.origin = None;
    cases.push(missing_origin);
    let mut foreign_origin = request(Some(admin.clone()));
    foreign_origin.origin = Some("https://foreign.example.test".into());
    cases.push(foreign_origin);
    for req in cases {
        let response = denied(&h.ctx(), &req);
        assert_eq!(response.status, unknown.status);
        assert_eq!(response.body, unknown.body);
        assert!(response.set_cookie.is_none());
    }
    *h.store.fail_user_role_lookup.borrow_mut() = true;
    assert_eq!(
        denied(&h.ctx(), &request(Some(admin.clone()))).body,
        unknown.body
    );
    *h.store.fail_user_role_lookup.borrow_mut() = false;
    *h.store.fail_session_lookup.borrow_mut() = true;
    assert_eq!(
        denied(&h.ctx(), &request(Some(admin.clone()))).body,
        unknown.body
    );
    *h.store.fail_session_lookup.borrow_mut() = false;
    h.store
        .user_roles
        .borrow_mut()
        .insert("admin1".into(), "member".into());
    assert_eq!(denied(&h.ctx(), &request(Some(admin))).body, unknown.body);
    assert!(h.store.ingest_jobs.borrow().is_empty());
    assert!(h.store.ingest_originals.borrow().is_empty());
}

#[test]
fn control_original_requires_current_session_row_and_fresh_admin_authentication() {
    for fault in ["missing", "revoked", "counter", "identity", "expired"] {
        let h = harness();
        let admin = seed_admin(&h, "admin1", "admin@example.com");
        let saved = h.store.sessions.borrow().values().next().unwrap().clone();
        if fault == "missing" {
            h.store.sessions.borrow_mut().clear();
        } else {
            let mut rows = h.store.sessions.borrow_mut();
            let row = rows.get_mut(&saved.id).unwrap();
            match fault {
                "revoked" => row.revoked = true,
                "counter" => row.rotation_counter += 1,
                "identity" => row.user_id = "member1".into(),
                "expired" => row.expires_at = NOW,
                _ => unreachable!(),
            }
        }
        let response = denied(&h.ctx(), &request(Some(admin)));
        assert_eq!(response.status, 404, "{fault}");
        assert_eq!(response.body, not_found().body, "{fault}");
    }
    let h = harness();
    let admin = seed_admin(&h, "admin1", "admin@example.com");
    seed_user(&h, "member1", "member@example.com");
    let member = magic_login(&h, "member@example.com");
    let mut ctx = h.ctx();
    ctx.now += session::PRIVILEGE_REAUTH_WINDOW_SECS + 1;
    let stale_admin = denied(&ctx, &request(Some(admin)));
    assert_eq!(stale_admin.status, 401);
    assert_eq!(
        stale_admin.body,
        session_error_response(session::SessionError::ReauthRequired).body
    );
    assert_eq!(denied(&ctx, &request(Some(member))).body, not_found().body);
}

#[test]
fn control_original_deleting_actor_is_refused_before_decryption() {
    let h = harness();
    let admin = seed_admin(&h, "admin1", "admin@example.com");
    h.store.deleting_users.borrow_mut().insert("admin1".into());
    let response = denied(&h.ctx(), &request(Some(admin.clone())));
    assert_eq!(response.status, 409);
    assert_eq!(response.body, account_deleting_response().body);
    h.store.deleting_users.borrow_mut().clear();
    *h.store.fail_account_deleting_lookup.borrow_mut() = true;
    let outage = denied(&h.ctx(), &request(Some(admin)));
    assert_eq!(outage.status, 500);
    assert_eq!(outage.body, internal().body);
    assert!(h.store.ingest_jobs.borrow().is_empty());
}

struct Repository {
    metadata: OriginalMetadata,
    envelope: Option<admin_ingest::IngestOriginalEnvelope>,
    confirm: Result<bool, OriginalError>,
    events: Vec<&'static str>,
}

impl Repository {
    fn new(h: &Harness) -> Self {
        // Reuse the existing synthetic generator; no new payload identities.
        let body = minimal_ingest_nzb();
        let hash = admin_ingest::content_sha256_hex(&body);
        let job = admin_ingest::derive_job_id(&body);
        let version = h.config.kek_version;
        let envelope = admin_ingest::encrypt_ingest_original(
            h.config.kek_by_version(version).unwrap(),
            version,
            &hash,
            &job,
            &body,
        )
        .unwrap();
        Self {
            metadata: OriginalMetadata {
                job_id: job,
                content_sha256: hash,
                nzb_bytes: body.len() as i64,
                envelope_version: admin_ingest::ENVELOPE_VERSION,
                payload_format: admin_ingest::PAYLOAD_FORMAT_RAW_NZB_V1,
                kek_version: version as i32,
                ciphertext_len: envelope.ciphertext.len() as i32,
                wrap_nonce_len: envelope.dek_wrapped_nonce.len() as i32,
                wrapped_dek_len: envelope.dek_wrapped.len() as i32,
                payload_nonce_len: envelope.payload_nonce.len() as i32,
            },
            envelope: Some(envelope),
            confirm: Ok(true),
            events: vec![],
        }
    }
}

impl OriginalRepository for Repository {
    async fn metadata(&mut self, job_id: &str) -> Result<Vec<OriginalMetadata>, OriginalError> {
        assert_eq!(job_id, self.metadata.job_id);
        self.events.push("metadata");
        Ok(vec![self.metadata.clone()])
    }

    async fn envelope(
        &mut self,
        _: &OriginalMetadata,
    ) -> Result<Option<admin_ingest::IngestOriginalEnvelope>, OriginalError> {
        self.events.push("envelope");
        Ok(self.envelope.take())
    }

    async fn confirm_current(&mut self, _: &OriginalMetadata) -> Result<bool, OriginalError> {
        self.events.push("confirm");
        self.confirm
    }
}

#[test]
fn control_original_binds_real_decrypted_original_to_session_actor_and_typed_context() {
    let h = harness();
    let admin = seed_admin(&h, "admin1", "admin@example.com");
    let audit_count = h.store.audits.borrow().len();
    for ip in ["198.51.100.9", "2001:db8::1", "untrusted header text"] {
        let mut repository = Repository::new(&h);
        let job = repository.metadata.job_id.clone();
        let mut ctx = h.ctx();
        ctx.source_ip = ip.into();
        let opened = block_on(read_admin_control_original(
            &ctx,
            &request(Some(admin.clone())),
            &job,
            &mut repository,
        ))
        .unwrap_or_else(|_| panic!("authorized original refused"));
        assert_eq!(repository.events, ["metadata", "envelope", "confirm"]);
        assert_eq!(opened.actor_user_id(), "admin1");
        assert_eq!(opened.source_ip(), ip.parse::<std::net::IpAddr>().ok());
        assert_eq!(opened.now(), NOW as i64);
        assert_eq!(opened.original().job_id(), job);
        assert!(opened.original().as_bytes() == minimal_ingest_nzb());
        assert_eq!(opened.original().parsed().files.len(), 1);
        // Use only this session-derived actor when calling the canonical SQL
        // adapter. This preparation itself cannot audit or mutate authority.
        let actor = nntune_ingest::admin_checkpoint_store::ControlActor {
            user_id: opened.actor_user_id(),
            source_ip: opened.source_ip(),
            now: opened.now(),
        };
        assert_eq!(actor.user_id, "admin1");
    }
    assert_eq!(h.store.audits.borrow().len(), audit_count);
    assert!(h.store.ingest_jobs.borrow().is_empty());
    assert!(h.store.ingest_originals.borrow().is_empty());
}

#[test]
fn control_original_storage_and_crypto_errors_return_no_holder_or_sensitive_detail() {
    let h = harness();
    let admin = seed_admin(&h, "admin1", "admin@example.com");
    for fault in ["missing", "store", "crypto", "metadata"] {
        let mut repository = Repository::new(&h);
        let job = repository.metadata.job_id.clone();
        match fault {
            "missing" => repository.confirm = Ok(false),
            "store" => repository.confirm = Err(OriginalError::Store),
            "crypto" => repository.envelope.as_mut().unwrap().ciphertext[0] ^= 1,
            "metadata" => repository.metadata.nzb_bytes += 1,
            _ => unreachable!(),
        }
        let response = match block_on(read_admin_control_original(
            &h.ctx(),
            &request(Some(admin.clone())),
            &job,
            &mut repository,
        )) {
            Err(response) => response,
            Ok(_) => panic!("invalid original opened"),
        };
        let expected = if fault == "missing" {
            not_found()
        } else {
            internal()
        };
        assert_eq!(response.status, expected.status);
        assert_eq!(response.body, expected.body);
        assert!(response.set_cookie.is_none());
        assert_eq!(
            repository.events.contains(&"confirm"),
            matches!(fault, "missing" | "store")
        );
    }
}
