use super::*;
use std::cell::RefCell;

struct Keys {
    requested: RefCell<Vec<u32>>,
    available: bool,
}
impl KekSource for Keys {
    fn kek(&self, version: u32) -> Option<&[u8; 32]> {
        self.requested.borrow_mut().push(version);
        (self.available && version == 7).then_some(&[9; 32])
    }
}
struct Authorization(bool);
impl OriginalAuthorization for Authorization {
    fn authorize(&self, _: &str) -> Result<(), OriginalError> {
        if self.0 {
            Ok(())
        } else {
            Err(OriginalError::Denied)
        }
    }
}
struct Repository {
    rows: Vec<OriginalMetadata>,
    envelope: Option<intake::IngestOriginalEnvelope>,
    running: bool,
    events: Vec<&'static str>,
}
impl OriginalRepository for Repository {
    async fn metadata(&mut self, _: &str) -> Result<Vec<OriginalMetadata>, OriginalError> {
        self.events.push("metadata");
        Ok(self.rows.clone())
    }
    async fn envelope(
        &mut self,
        _: &OriginalMetadata,
    ) -> Result<Option<intake::IngestOriginalEnvelope>, OriginalError> {
        self.events.push("envelope");
        Ok(self.envelope.clone())
    }
    async fn mark_running(&mut self, _: &OriginalMetadata) -> Result<bool, OriginalError> {
        self.events.push("running");
        Ok(self.running)
    }
}
fn fixture() -> (Vec<u8>, String, Repository, Keys) {
    // Reuse the existing synthetic corpus; never add payload/subject/segment
    // literals, and never print parsed content or plaintext in assertions.
    let body = intake::tests::minimal_nzb();
    let job = intake::derive_job_id(&body);
    let hash = intake::content_sha256_hex(&body);
    let envelope = intake::encrypt_ingest_original(&[9; 32], 7, &hash, &job, &body).unwrap();
    let row = OriginalMetadata {
        job_id: job.clone(),
        content_sha256: hash,
        nzb_bytes: body.len() as i64,
        envelope_version: 2,
        payload_format: 1,
        kek_version: 7,
        ciphertext_len: envelope.ciphertext.len() as i32,
        wrap_nonce_len: 12,
        wrapped_dek_len: 48,
        payload_nonce_len: 12,
    };
    (
        body,
        job,
        Repository {
            rows: vec![row],
            envelope: Some(envelope),
            running: true,
            events: vec![],
        },
        Keys {
            requested: RefCell::new(vec![]),
            available: true,
        },
    )
}

#[tokio::test]
async fn opens_only_after_authorization_and_persists_running_before_return() {
    let (body, job, mut repo, keys) = fixture();
    for _ in 0..2 {
        let opened = read_original(&Authorization(true), &mut repo, &keys, &job)
            .await
            .unwrap();
        assert!(opened.as_bytes() == body.as_slice());
    }
    assert_eq!(
        repo.events,
        ["metadata", "envelope", "running", "metadata", "envelope", "running"]
    );
    assert_eq!(*keys.requested.borrow(), [7, 7]);
}

#[tokio::test]
async fn denial_and_invalid_request_stop_before_storage_or_keys() {
    let (_, job, mut repo, keys) = fixture();
    assert!(matches!(
        read_original(&Authorization(false), &mut repo, &keys, &job).await,
        Err(OriginalError::Denied)
    ));
    for invalid in [
        String::new(),
        "adm_".into(),
        "a".repeat(1000),
        format!("adm_{}", "é".repeat(22)),
    ] {
        assert!(matches!(
            read_original(&Authorization(true), &mut repo, &keys, &invalid).await,
            Err(OriginalError::Invalid)
        ));
    }
    assert!(repo.events.is_empty());
    assert!(keys.requested.borrow().is_empty());
}

#[tokio::test]
async fn metadata_failures_never_materialize_ciphertext() {
    let mutations: Vec<fn(&mut OriginalMetadata)> = vec![
        |r| r.job_id.push('x'),
        |r| r.content_sha256 = "A".repeat(64),
        |r| r.nzb_bytes = 0,
        |r| r.nzb_bytes = intake::ADMIN_INGEST_BODY_LIMIT_BYTES as i64 + 1,
        |r| r.ciphertext_len = i32::MAX,
        |r| r.envelope_version = 1,
        |r| r.payload_format = 2,
        |r| r.kek_version = -1,
        |r| r.wrap_nonce_len = 13,
        |r| r.wrapped_dek_len = 47,
        |r| r.payload_nonce_len = 0,
    ];
    for mutate in mutations {
        let (_, job, mut repo, keys) = fixture();
        mutate(&mut repo.rows[0]);
        assert!(matches!(
            read_original(&Authorization(true), &mut repo, &keys, &job).await,
            Err(OriginalError::Invalid)
        ));
        assert_eq!(repo.events, ["metadata"]);
        assert!(keys.requested.borrow().is_empty());
    }
    for duplicate in [false, true] {
        let (_, job, mut repo, keys) = fixture();
        if duplicate {
            repo.rows.push(repo.rows[0].clone());
        } else {
            repo.rows.clear();
        }
        assert!(read_original(&Authorization(true), &mut repo, &keys, &job)
            .await
            .is_err());
        assert_eq!(repo.events, ["metadata"]);
    }
}

#[tokio::test]
async fn stored_key_is_pinned_with_no_fallback() {
    let (_, job, mut repo, mut keys) = fixture();
    keys.available = false;
    assert!(matches!(
        read_original(&Authorization(true), &mut repo, &keys, &job).await,
        Err(OriginalError::UnknownKey)
    ));
    assert_eq!(*keys.requested.borrow(), [7]);
    assert_eq!(repo.events, ["metadata"]);
}

#[tokio::test]
async fn corrupt_swapped_or_missing_envelopes_never_mark_running() {
    for mutation in 0..5 {
        let (_, job, mut repo, keys) = fixture();
        match mutation {
            0 => repo.envelope = None,
            1 => repo.envelope.as_mut().unwrap().ciphertext[0] ^= 1,
            2 => repo.envelope.as_mut().unwrap().ciphertext.push(0),
            3 => repo
                .envelope
                .as_mut()
                .unwrap()
                .dek_wrapped
                .pop()
                .map(|_| ())
                .unwrap(),
            _ => repo.rows[0].content_sha256 = "b".repeat(64),
        }
        assert!(read_original(&Authorization(true), &mut repo, &keys, &job)
            .await
            .is_err());
        assert_eq!(repo.events, ["metadata", "envelope"]);
    }
}

#[tokio::test]
async fn authenticated_but_false_content_identity_is_rejected_before_running() {
    let (body, job, mut repo, keys) = fixture();
    let false_hash = "c".repeat(64);
    repo.envelope =
        Some(intake::encrypt_ingest_original(&[9; 32], 7, &false_hash, &job, &body).unwrap());
    repo.rows[0].content_sha256 = false_hash;
    assert!(matches!(
        read_original(&Authorization(true), &mut repo, &keys, &job).await,
        Err(OriginalError::Invalid)
    ));
    assert_eq!(repo.events, ["metadata", "envelope"]);
}

#[tokio::test]
async fn lost_active_row_discards_plaintext_instead_of_executing() {
    let (_, job, mut repo, keys) = fixture();
    repo.running = false;
    assert!(matches!(
        read_original(&Authorization(true), &mut repo, &keys, &job).await,
        Err(OriginalError::Missing)
    ));
    assert_eq!(repo.events, ["metadata", "envelope", "running"]);
}

#[test]
fn sql_pins_active_admin_identity_and_bounds_bytes_before_loading() {
    for sql in [
        original_metadata_sql(),
        original_envelope_sql(),
        mark_running_sql(),
    ] {
        assert!(sql.contains("j.source = 'admin'"));
        assert!(sql.contains("j.status IN ('queued', 'running')"));
        assert!(sql.contains("j.id = $1"));
        assert!(sql.contains("j.idempotency_key = 'admin:' ||"));
        assert!(sql.contains("j.content_sha256 ="));
    }
    assert!(!original_metadata_sql().contains("SELECT o.nzb_ref_encrypted"));
    let sql = original_envelope_sql();
    assert!(sql.contains("octet_length(o.nzb_ref_encrypted) BETWEEN 17 AND 8388624"));
    assert!(sql.contains("o.nzb_ref_kek_version = $3"));
    assert!(sql.contains("o.nzb_bytes = $4"));
    assert!(sql.contains("LIMIT 1"));
    assert!(!mark_running_sql().contains("SET status = $"));
}
