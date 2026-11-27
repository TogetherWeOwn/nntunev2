use futures_util::FutureExt;
use nntune_api::{
    admin_ingest::{
        self, decrypt_ingest_original, encrypt_ingest_original, IngestOriginalEnvelope,
        ADMIN_INGEST_BODY_LIMIT_BYTES, ENVELOPE_VERSION, PAYLOAD_FORMAT_RAW_NZB_V1,
    },
    ledger::{self, LedgerError, ReplayOutcome},
    listen_history::{self, ListenEvent, ListenEventBody},
    postgres::PgClient,
    vault::VaultError,
};
use tokio::io::AsyncWriteExt;
use tokio_postgres::{config::SslMode, Config, NoTls};

#[test]
fn admin_original_encrypt_requires_the_content_hash_and_job_of_the_bytes() {
    let kek = [7; 32];
    let body = b"<nzb>synthetic content</nzb>";
    let other = b"<nzb>other synthetic content</nzb>";
    let hash = admin_ingest::content_sha256_hex(body);
    let job = admin_ingest::derive_job_id(body);
    let other_hash = admin_ingest::content_sha256_hex(other);
    let other_job = admin_ingest::derive_job_id(other);
    for (hash, job) in [(&other_hash, &job), (&hash, &other_job)] {
        assert!(matches!(
            encrypt_ingest_original(&kek, 1, hash, job, body),
            Err(VaultError::Invalid)
        ));
    }
    let envelope = encrypt_ingest_original(&kek, 1, &hash, &job, body).unwrap();
    let open = |envelope: &IngestOriginalEnvelope| {
        decrypt_ingest_original(
            &kek,
            &hash,
            &job,
            ENVELOPE_VERSION,
            PAYLOAD_FORMAT_RAW_NZB_V1,
            1,
            envelope,
        )
    };
    assert_eq!(open(&envelope).unwrap(), body);
    let mut invalid = envelope.clone();
    invalid.ciphertext.truncate(16);
    assert_eq!(open(&invalid), Err(VaultError::CorruptStoredData));
    invalid
        .ciphertext
        .resize(ADMIN_INGEST_BODY_LIMIT_BYTES + 17, 0);
    assert_eq!(open(&invalid), Err(VaultError::CorruptStoredData));
    for out_of_bounds in [&[][..], &invalid.ciphertext[..]] {
        let hash = admin_ingest::content_sha256_hex(out_of_bounds);
        let job = admin_ingest::derive_job_id(out_of_bounds);
        assert!(matches!(
            encrypt_ingest_original(&kek, 1, &hash, &job, out_of_bounds),
            Err(VaultError::Invalid)
        ));
    }
}

#[test]
fn admin_list_offsets_fit_the_database_type() {
    let max = i64::MAX as u64;
    for (raw, expected) in [
        (max - 1, max - 1),
        (max, max),
        (max + 1, max),
        (u64::MAX, max),
    ] {
        assert_eq!(
            admin_ingest::parse_list_window(Some("1"), Some(&raw.to_string())),
            (1, expected)
        );
    }
    assert_eq!(
        admin_ingest::parse_list_window(None, Some("invalid")),
        (50, 0)
    );
}

#[test]
fn ledger_rejects_unrepresentable_metadata_without_truncating_the_duration() {
    let max = i64::MAX as u64;
    assert_eq!(
        ledger::resolve_duration_ms(None, None, Some(8), Some(max)),
        Some(max)
    );
    for bytes in [max + 1, u64::MAX] {
        assert_eq!(
            ledger::resolve_duration_ms(None, None, Some(7), Some(bytes)),
            None
        );
    }

    let window =
        |size, duration| ledger::prepare_play_window("user", "track", "play", 1, size, duration, 1);
    let valid = window(Some(max), Some(max)).unwrap();
    assert_eq!(valid.file_bytes, Some(max));
    assert_eq!(valid.duration_ms, Some(max));
    for (size, duration) in [(Some(max + 1), Some(max)), (Some(max), Some(max + 1))] {
        assert_eq!(window(size, duration), Err(LedgerError::Invalid));
    }
    assert!(window(Some(max), None).unwrap().rate_fallback);
}

#[test]
fn ledger_replay_admits_only_valid_intents_and_preserves_the_owner_binding() {
    for (user, track) in [("u\0ser", "track"), ("user", "tr\0ack")] {
        assert_eq!(
            ledger::prepare_ledger_row(user, track, "legacy", 1, 1, 1),
            Err(LedgerError::Invalid)
        );
    }
    assert!(ledger::prepare_ledger_row("Usér", "träck", "legacy", 1, 1, 1).is_ok());
    let valid = ledger::prepare_play_ledger_row("User", "Track", "Play", 1, 1, 1).unwrap();
    assert_eq!(
        ledger::classify_fenced_replay(None, &valid, true),
        ReplayOutcome::AccountDeleting
    );
    assert_eq!(
        ledger::classify_fenced_replay(Some(&valid), &valid, true),
        ReplayOutcome::Duplicate
    );

    for invalid in [
        ledger::LedgerRow {
            track_id: "Other".into(),
            ..valid.clone()
        },
        ledger::LedgerRow {
            period_month: "1970-02".into(),
            ..valid.clone()
        },
        ledger::LedgerRow {
            bytes_served: 0,
            ..valid.clone()
        },
    ] {
        assert_eq!(
            ledger::classify_replay(None, &invalid),
            ReplayOutcome::Conflict
        );
        assert_eq!(
            ledger::classify_replay(Some(&invalid), &invalid),
            ReplayOutcome::Conflict
        );
        assert_eq!(
            ledger::classify_fenced_replay(None, &invalid, true),
            ReplayOutcome::Conflict
        );
    }
}

#[tokio::test]
async fn listen_record_rejects_invalid_identities_before_opening_a_transaction() {
    let (transport, mut peer) = tokio::io::duplex(1024);
    // AuthenticationOk and ReadyForQuery are the entire synthetic startup reply.
    peer.write_all(b"R\0\0\0\x08\0\0\0\0Z\0\0\0\x05I")
        .await
        .unwrap();
    let (client, _connection) = Config::new()
        .user("fixture")
        .ssl_mode(SslMode::Disable)
        .connect_raw(transport, NoTls)
        .await
        .unwrap();
    let mut client = PgClient::new(client);
    let mut event = ListenEventBody {
        playback_id: "40c325c0-2cee-489d-a8b6-fbe937155671".into(),
        track_id: String::new(),
        event: ListenEvent::Start,
    };
    // The connection stays unpolled. An attempted transaction would pend.
    for track in ["".into(), "é".repeat(65), "track\u{85}id".into()] {
        event.track_id = track;
        let result = listen_history::record(&mut client, "owner", &event, 1);
        assert!(matches!(result.now_or_never(), Some(Err(_))));
    }
    event.track_id = "é".repeat(64);
    event.playback_id.make_ascii_uppercase();
    let result = listen_history::record(&mut client, "owner", &event, 1);
    assert!(matches!(result.now_or_never(), Some(Err(_))));
    event.playback_id.make_ascii_lowercase();
    let result = listen_history::record(&mut client, "owner", &event, 1);
    assert!(result.now_or_never().is_none());
}
