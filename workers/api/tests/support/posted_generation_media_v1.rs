//! Synthetic sealed buffers test persistence, not header authentication or
//! production promotion. The retained state below is explicitly fixture-seeded.
use super::*;
use nntune_ingest::posted_files::{PostedFileMedia, MEDIA_V1_KEY_MODE};
use nntune_ingest::posted_generations::{prepare_retained_check, RetainedReferenceKind};

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
