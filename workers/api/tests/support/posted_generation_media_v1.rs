//! Shape/replay controls use synthetic sealed buffers; the keyring control uses
//! real engine/key-envelope crypto with synthetic plaintext and manifest buffers.
//! No production promotion is proved; retained states are explicitly seeded.
use super::*;
use nntune_ingest::posted_files::{PostedFileMedia, MEDIA_V1_KEY_MODE};
use nntune_ingest::posted_generations::{prepare_retained_check, RetainedReferenceKind};

#[test]
fn postgres_media_v1_keyring_open_authenticates_the_recovered_candidate_ciphertext() {
    run_test(async {
        use nntune_api::{posted_media, rotation::KekSource};
        use sha2::{Digest, Sha256};
        use zeroize::Zeroizing;

        struct Keys(Zeroizing<[u8; 32]>);
        impl KekSource for Keys {
            fn kek(&self, version: u32) -> Option<&[u8; 32]> {
                (version == 1).then_some(&self.0)
            }
        }
        let keys = Keys(Zeroizing::new(
            nntune_api::random::secure_random_bytes().unwrap(),
        ));
        let plaintext = vec![17; 1024];
        let digest: [u8; 32] = Sha256::digest(&plaintext).into();
        let mut db = connect().await;
        let mut c = case(&db).await;
        c.sealed.hash = digest.iter().map(|b| format!("{b:02x}")).collect();
        let s = &c.sealed;
        let media = posted_media::mint(&keys, 1, &s.hash).expect("mint media key");
        let mut mint = s.mint();
        mint.custody.article_count = 1;
        mint.custody.article_payload_size = 1040;
        mint.custody.media = PostedFileMedia::MediaV1 {
            key_mode: MEDIA_V1_KEY_MODE,
            header: &media.header,
        };
        let envelope = &media.sealed_key.envelope;
        mint.custody.file_key_encrypted = &envelope.ciphertext;
        mint.custody.file_key_dek_wrapped_nonce = &envelope.dek_wrapped_nonce;
        mint.custody.file_key_dek_wrapped = &envelope.dek_wrapped;
        mint.custody.file_key_payload_nonce = &envelope.payload_nonce;
        let attempt = acquire(&mut db, &c.job, &c.owner, 0).await;
        let expected = observe(&db, &s.hash).await.expect("observe media hash");
        assert!(matches!(
            admit(&mut db, &attempt, &expected, &mint).await,
            Ok(AdmissionOutcome::Admitted(..))
        ));
        let before = snapshot(&db, s).await;
        drop(db);
        let mut db = connect().await;
        let Ok(AdmissionOutcome::ResumedSameCandidate(_, bundle)) =
            recover(&mut db, &attempt, &s.hash, &s.op).await
        else {
            panic!("recover authenticated media custody")
        };
        let stored = bundle.custody().expect("decode recovered media custody");
        let mut stored_key = media.sealed_key.clone();
        stored_key.kek_version = stored.file_key_kek_version.try_into().unwrap();
        stored_key.payload_format = stored.file_key_payload_format.try_into().unwrap();
        stored_key.envelope.ciphertext = stored.file_key_encrypted.to_vec();
        stored_key.envelope.dek_wrapped_nonce =
            stored.file_key_dek_wrapped_nonce.try_into().unwrap();
        stored_key.envelope.dek_wrapped = stored.file_key_dek_wrapped.to_vec();
        stored_key.envelope.payload_nonce = stored.file_key_payload_nonce.try_into().unwrap();
        let verified = posted_media::open(
            &keys,
            stored.plaintext_sha256,
            stored.media.format(),
            stored.media.key_mode().expect("stored random mode"),
            stored.media.header().expect("stored media header"),
            &stored_key,
        )
        .expect("authenticate recovered key and header");
        let wire = media
            .sealer
            .seal_to_vec(&plaintext)
            .expect("seal synthetic media");
        assert_eq!(
            verified
                .reader(stored.plaintext_size.try_into().unwrap())
                .unwrap()
                .verify_all(&wire)
                .unwrap(),
            digest
        );
        assert!(before == snapshot(&db, s).await);
    });
}

#[test]
fn postgres_media_v1_candidate_recovery_replay_and_retained_custody_keep_exact_metadata() {
    run_test(async {
        let mut db = connect().await;
        let c = case(&db).await;
        let s = &c.sealed;
        let header = [0x51; 48];
        let attempt = acquire(&mut db, &c.job, &c.owner, 0).await;
        let mut mint = s.mint();
        mint.custody.media = PostedFileMedia::MediaV1 {
            key_mode: MEDIA_V1_KEY_MODE,
            header: &header,
        };
        let expected = observe(&db, &s.hash).await.expect("observe absent media");
        let outcome = admit(&mut db, &attempt, &expected, &mint)
            .await
            .expect("admit media");
        let AdmissionOutcome::Admitted(_, bundle) = outcome else {
            panic!("media candidate not admitted")
        };
        assert!(bundle.custody().expect("media custody") == mint.custody);
        let before = snapshot(&db, s).await;
        for outcome in [
            admit(&mut db, &attempt, &expected, &mint).await,
            recover(&mut db, &attempt, &s.hash, &s.op).await,
        ] {
            let Ok(AdmissionOutcome::ResumedSameCandidate(_, stored)) = outcome else {
                panic!("media replay failed")
            };
            assert!(stored.custody().expect("replayed media") == mint.custody);
        }
        assert!(before == snapshot(&db, s).await);
        let different_header = [0x52; 48];
        let mut divergent = s.mint();
        divergent.custody.media = PostedFileMedia::MediaV1 {
            key_mode: MEDIA_V1_KEY_MODE,
            header: &different_header,
        };
        assert!(matches!(
            admit(&mut db, &attempt, &expected, &divergent).await,
            Ok(AdmissionOutcome::StaleOrDiverged)
        ));
        for sql in [
            "UPDATE posted_files SET media_header = decode(repeat('00', 48), 'hex') WHERE id = $1",
            "UPDATE posted_files SET media_key_mode = 'convergent' WHERE id = $1",
            "UPDATE posted_files SET media_format = 'legacy' WHERE id = $1",
        ] {
            assert!(db.execute(sql, &[&s.generation]).await.is_err());
            assert!(before == snapshot(&db, s).await);
        }
        db.batch_execute(include_str!(
            "../../../../migrations/0050_posted_file_media_v1.sql"
        ))
        .await
        .quiet("replay media migration");
        assert!(before == snapshot(&db, s).await);

        // Fixture-only promotion/reference state, not an implemented publisher.
        db.execute(
            "UPDATE posted_file_operations SET state = 'promoted' WHERE id = $1",
            &[&s.op],
        )
        .await
        .quiet("seed retained operation");
        db.execute("UPDATE posted_file_registry SET selected_generation = $2, pending_operation = NULL, revision = revision + 1 WHERE plaintext_sha256 = $1", &[&s.hash, &s.generation])
            .await.quiet("seed retained selection");
        db.execute("INSERT INTO posted_file_references (kind, owner_id, generation_id, plaintext_sha256) VALUES ('serving', $1, $2, $3)", &[&c.owner, &s.generation, &s.hash])
            .await.quiet("seed retained reference");
        db.execute("UPDATE posted_file_health SET state = 'usable', round = 1, revision = revision + 1, verified_at = 1700000000, retention_checked_at = 1700000000 WHERE generation_id = $1", &[&s.generation])
            .await.quiet("seed retained health");
        let before = snapshot(&db, s).await;
        let Some((permit, custody)) = prepare_retained_check(
            &mut db,
            &attempt,
            &s.generation,
            RetainedReferenceKind::Serving,
            &c.owner,
        )
        .await
        .expect("prepare retained media") else {
            panic!("retained media refused")
        };
        assert!(custody.custody().expect("retained media custody") == mint.custody);
        drop((permit, custody));
        assert!(before == snapshot(&db, s).await);
    });
}
