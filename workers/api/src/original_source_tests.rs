//! File-backed synthetic persistence, real pure producer + API envelope +
//! bounded reader. SQLite dialect translation is not Hyperdrive runtime proof.
use super::*;
use nntune_core::{
    nzb::{File, Segment},
    original_source::{RangeHint, SourceError},
};
use nntune_ingest::{
    matching::MbRecording,
    track_writers::{self, SourceTrack},
};
use rusqlite::{params, Connection, OptionalExtension};

const A: &str = "00000000-0000-0000-0000-000000000001";
const B: &str = "00000000-0000-0000-0000-000000000002";
const RELEASE: &str = "00000000-0000-0000-0000-000000000003";
const RECORDING: &str = "00000000-0000-0000-0000-000000000004";
const RECORDING_B: &str = "00000000-0000-0000-0000-000000000005";
const JOB: &str = "job_generated";

struct Keys;
impl KekSource for Keys {
    fn kek(&self, version: u32) -> Option<&[u8; 32]> {
        (version == 7).then_some(&[19u8; 32])
    }
}
struct Allow;
impl SourceReadAuthorization for Allow {
    fn authorize(&self, _: &str) -> Result<(), SourceServiceError> {
        Ok(())
    }
}
struct Deny;
impl SourceReadAuthorization for Deny {
    fn authorize(&self, _: &str) -> Result<(), SourceServiceError> {
        Err(SourceServiceError::Denied)
    }
}

fn files() -> Vec<File> {
    (0..4)
        .map(|i| File {
            ordinal: i,
            subject: String::new(),
            filename: None,
            role: if i % 2 == 0 { "nfo" } else { "content" }.into(),
            container: if i % 2 == 0 { "other" } else { "flac" }.into(),
            posted_at_unix: 0,
            segments: vec![Segment {
                ordinal: 0,
                number: 1,
                message_id: format!("generated-file-{i}@fixture.invalid"),
                encoded_bytes: 128,
            }],
        })
        .collect()
}
fn recordings() -> Vec<MbRecording> {
    vec![
        MbRecording {
            mbid: RECORDING.into(),
            title: "Generated one".into(),
            medium_no: Some(1),
            track_no: Some(1),
        },
        MbRecording {
            mbid: RECORDING_B.into(),
            title: "Generated two".into(),
            medium_no: Some(1),
            track_no: Some(2),
        },
    ]
}
fn selected(id: &str, position: i32) -> SourceTrack<'_> {
    SourceTrack {
        id,
        release_id: RELEASE,
        medium_no: 1,
        track_no: position,
        recording_id: if position == 1 {
            RECORDING
        } else {
            RECORDING_B
        },
    }
}
fn prepare(id: &str, position: i32, job: &str) -> OriginalSource {
    track_writers::original_source_for_track(
        job,
        RELEASE,
        &files(),
        &recordings(),
        &selected(id, position),
        3,
        Some(RangeHint {
            part_size: 3,
            final_begin: 0,
            final_end: 3,
        }),
    )
    .unwrap()
}
fn sql(raw: &str) -> String {
    raw.replace("::text::uuid", "")
        .replace("::text", "")
        .replace("octet_length", "length")
        .replace(" FOR SHARE", "")
        // SQLite's INSERT SELECT parser needs WHERE before ON CONFLICT.
        .replace(
            "AS writer_fence ON CONFLICT",
            "AS writer_fence WHERE true ON CONFLICT",
        )
}

struct TempDb(std::path::PathBuf);
impl TempDb {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "source-conformance-{}.sqlite",
            crate::providers::mint_provider_id().unwrap()
        ));
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap();
        Self(path)
    }
}
impl Drop for TempDb {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}
struct Store {
    db: Connection,
    metadata_calls: usize,
    envelope_calls: usize,
}
impl Store {
    fn open(path: &std::path::Path) -> Self {
        Self {
            db: Connection::open(path).unwrap(),
            metadata_calls: 0,
            envelope_calls: 0,
        }
    }
    fn init(&self) {
        self.db.execute_batch("PRAGMA foreign_keys=ON;
            CREATE TABLE ingest_jobs(id TEXT PRIMARY KEY);
            CREATE TABLE tracks(id TEXT PRIMARY KEY, release_id TEXT, medium_no INTEGER, track_no INTEGER, mbid TEXT, title TEXT, UNIQUE(release_id,medium_no,track_no));
            CREATE TABLE track_sources(track_id TEXT PRIMARY KEY REFERENCES tracks(id), source_job_id TEXT REFERENCES ingest_jobs(id), indexer_id TEXT, nzb_ref_encrypted BLOB, nzb_ref_envelope_version INTEGER, nzb_ref_kek_version INTEGER, nzb_ref_dek_wrapped_nonce BLOB, nzb_ref_dek_wrapped BLOB, nzb_ref_payload_nonce BLOB, nzb_ref_payload_format INTEGER);
        ").unwrap();
        self.db
            .execute("INSERT INTO ingest_jobs VALUES ($1)", [JOB])
            .unwrap();
        for (id, pos, rec) in [(A, 1, RECORDING), (B, 2, RECORDING_B)] {
            self.db
                .execute(
                    "INSERT INTO tracks VALUES ($1,$2,1,$3,$4,'Generated')",
                    params![id, RELEASE, pos, rec],
                )
                .unwrap();
        }
    }
    fn winner(&self, position: i32) -> (String, String, i32, i32, String) {
        self.db
            .query_row(
                &sql(track_writers::select_track_for_source_sql()),
                params![RELEASE, 1, position],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .unwrap()
    }
    fn publish(&mut self, sealed: &SealedSource) -> usize {
        let e = &sealed.envelope;
        let m = &sealed.metadata;
        self.db
            .execute(
                &sql(track_writers::upsert_track_source_sql()),
                params![
                    m.track_id,
                    m.source_job_id,
                    Option::<String>::None,
                    e.ciphertext,
                    m.kek_version,
                    e.dek_wrapped_nonce.as_slice(),
                    e.dek_wrapped,
                    e.payload_nonce.as_slice(),
                    m.payload_format
                ],
            )
            .unwrap()
    }
}
impl SourceRepository for Store {
    fn metadata(&mut self, track: &str) -> Result<[Option<SourceMetadata>; 2], SourceServiceError> {
        self.metadata_calls += 1;
        let mut stmt = self
            .db
            .prepare(&sql(source_metadata_sql()))
            .map_err(|_| SourceServiceError::Store)?;
        let rows = stmt
            .query_map([track], |r| {
                Ok(SourceMetadata {
                    track_id: r.get(0)?,
                    source_job_id: r.get(1)?,
                    envelope_version: r.get(2)?,
                    payload_format: r.get(3)?,
                    kek_version: r.get(4)?,
                    ciphertext_len: r.get(5)?,
                    wrap_nonce_len: r.get(6)?,
                    wrapped_dek_len: r.get(7)?,
                    payload_nonce_len: r.get(8)?,
                })
            })
            .map_err(|_| SourceServiceError::Store)?;
        let mut out = [None, None];
        for (i, row) in rows.enumerate() {
            if i >= 2 {
                return Err(SourceServiceError::Invalid);
            }
            out[i] = Some(row.map_err(|_| SourceServiceError::Invalid)?);
        }
        Ok(out)
    }
    fn envelope(
        &mut self,
        m: &SourceMetadata,
    ) -> Result<Option<FieldEnvelope>, SourceServiceError> {
        self.envelope_calls += 1;
        self.db
            .query_row(
                &sql(source_envelope_sql()),
                params![m.track_id, m.source_job_id, m.kek_version, m.ciphertext_len],
                |r| {
                    let wrap_nonce: Vec<u8> = r.get(0)?;
                    let payload_nonce: Vec<u8> = r.get(2)?;
                    Ok(FieldEnvelope {
                        dek_wrapped_nonce: wrap_nonce.try_into().unwrap(),
                        dek_wrapped: r.get(1)?,
                        payload_nonce: payload_nonce.try_into().unwrap(),
                        ciphertext: r.get(3)?,
                    })
                },
            )
            .optional()
            .map_err(|_| SourceServiceError::Store)
    }
}
fn persisted() -> (TempDb, Store) {
    let temp = TempDb::new();
    let mut s = Store::open(&temp.0);
    s.init();
    let sealed = seal_original_source(&Keys, 7, &prepare(A, 1, JOB)).unwrap();
    assert_eq!(s.publish(&sealed), 1);
    (temp, s)
}

#[test]
fn metadata_matches_postgres_int4_projection_and_snapshot_parameters() {
    use tokio_postgres::types::{FromSql, ToSql, Type};

    fn int4<T>(value: T, wire_value: i32)
    where
        T: for<'a> FromSql<'a> + ToSql + PartialEq + std::fmt::Debug,
    {
        assert!(<T as FromSql>::accepts(&Type::INT4));
        assert!(<T as ToSql>::accepts(&Type::INT4));
        assert!(!<T as FromSql>::accepts(&Type::INT8));
        assert!(!<T as ToSql>::accepts(&Type::INT8));
        let decoded = T::from_sql(&Type::INT4, &wire_value.to_be_bytes()).unwrap();
        assert_eq!(value, decoded);
    }

    let source = prepare(A, 1, JOB);
    let metadata = seal_original_source(&Keys, 7, &source).unwrap().metadata;
    // INTEGER KEK version and octet_length(BYTEA) are PostgreSQL int4;
    // the same selected values bind $3/$4 of source_envelope_sql.
    int4(metadata.kek_version, 7);
    int4(
        metadata.ciphertext_len,
        (source.encode().unwrap().len() + 16).try_into().unwrap(),
    );
    int4(metadata.wrap_nonce_len, 12);
    int4(metadata.wrapped_dek_len, 48);
    int4(metadata.payload_nonce_len, 12);
}

#[test]
fn seal_rejects_kek_versions_outside_the_persisted_integer_domain() {
    struct AnyVersion;
    impl KekSource for AnyVersion {
        fn kek(&self, _: u32) -> Option<&[u8; 32]> {
            Some(&[19; 32])
        }
    }
    let source = prepare(A, 1, JOB);
    for version in [i32::MAX as u32 + 1, u32::MAX] {
        assert!(matches!(
            seal_original_source(&AnyVersion, version, &source),
            Err(SourceServiceError::Invalid)
        ));
    }
    let sealed = seal_original_source(&AnyVersion, i32::MAX as u32, &source).unwrap();
    assert_eq!(sealed.metadata.validate(A), Ok(i32::MAX as u32));
}

#[test]
fn persisted_winner_producer_seal_reopen_reader_is_per_backed_file() {
    let (temp, mut store) = persisted();
    // Resolve the actual persisted winner, never mint a pre-insert UUID.
    let (id, release, medium_no, track_no, recording_id) = store.winner(2);
    let selected = SourceTrack {
        id: &id,
        release_id: &release,
        medium_no,
        track_no,
        recording_id: &recording_id,
    };
    let mut unordered = files();
    unordered.reverse();
    let b = track_writers::original_source_for_track(
        JOB,
        RELEASE,
        &unordered,
        &recordings(),
        &selected,
        3,
        None,
    )
    .unwrap();
    let sealed_b = seal_original_source(&Keys, 7, &b).unwrap();
    assert_eq!(store.publish(&sealed_b), 1);
    drop(store);
    let mut store = Store::open(&temp.0);
    let a = read_original_source(&Allow, &mut store, &Keys, A).unwrap();
    let b = read_original_source(&Allow, &mut store, &Keys, B).unwrap();
    assert!(a.file_ordinal() == 1 && b.file_ordinal() == 3);
    assert!(a.article(0).unwrap().reference() == "generated-file-1@fixture.invalid");
    assert!(b.article(0).unwrap().reference() == "generated-file-3@fixture.invalid");
    // Read the persisted source through the actual bounded media seam.
    struct Body {
        opened: Vec<String>,
    }
    impl nntune_core::original_source::reader::ArticleSource for Body {
        type Body = std::io::Cursor<Vec<u8>>;
        fn open(&mut self, reference: &str) -> Result<Self::Body, SourceError> {
            self.opened.push(reference.into());
            let mut bytes = b"=ybegin line=128 size=3 name=generated\r\n".to_vec();
            nntune_core::yenc::encode_body(&mut bytes, &[0, 1, 2]).unwrap();
            bytes.extend_from_slice(b"\r\n=yend size=3\r\n");
            Ok(std::io::Cursor::new(bytes))
        }
    }
    struct Control;
    impl nntune_core::original_source::reader::ReadControl for Control {
        fn check(&mut self) -> Result<(), SourceError> {
            Ok(())
        }
    }
    let mut body = Body { opened: Vec::new() };
    let mut out = Vec::new();
    nntune_core::original_source::reader::read_range(&a, 1..3, &mut body, &mut Control, |bytes| {
        out.extend_from_slice(bytes);
        Ok(())
    })
    .unwrap();
    assert_eq!(out, vec![1, 2]);
    assert!(body.opened.len() == 1 && body.opened[0] == a.article(0).unwrap().reference());
}

#[test]
fn first_writer_wins_keeps_the_complete_original_binding_and_hint() {
    let (_temp, mut store) = persisted();
    store
        .db
        .execute("INSERT INTO ingest_jobs VALUES ('job_later')", [])
        .unwrap();
    let mut later_files = files();
    later_files[1].role = "nfo".into();
    let (id, release, medium_no, track_no, recording_id) = store.winner(1);
    let winner = SourceTrack {
        id: &id,
        release_id: &release,
        medium_no,
        track_no,
        recording_id: &recording_id,
    };
    let later_source = track_writers::original_source_for_track(
        "job_later",
        RELEASE,
        &later_files,
        &recordings(),
        &winner,
        3,
        None,
    )
    .unwrap();
    assert_eq!(later_source.file_ordinal(), 3);
    let later = seal_original_source(&Keys, 7, &later_source).unwrap();
    assert_eq!(store.publish(&later), 0);
    let before = read_original_source(&Allow, &mut store, &Keys, A).unwrap();
    assert!(
        before.source_job_id() == JOB
            && before.file_ordinal() == 1
            && before.range_hint().is_some()
    );
    let replay = seal_original_source(&Keys, 7, &prepare(A, 1, JOB)).unwrap();
    assert_eq!(store.publish(&replay), 0);
    // A non-persisted job fences even a validly sealed source to zero rows.
    let unpersisted = seal_original_source(&Keys, 7, &prepare(B, 2, "job_absent")).unwrap();
    assert_eq!(store.publish(&unpersisted), 0);
    assert!(read_original_source(&Allow, &mut store, &Keys, B).is_err());
}

#[test]
fn producer_accepts_uuid_case_normalized_by_the_persisted_projection() {
    let release = "aaaaaaaa-0000-0000-0000-000000000003";
    let recording = "bbbbbbbb-0000-0000-0000-000000000004";
    let mut r = recordings();
    r[0].mbid = recording.to_ascii_uppercase();
    let winner = SourceTrack {
        id: A,
        release_id: release,
        medium_no: 1,
        track_no: 1,
        recording_id: recording,
    };
    let prepared = track_writers::original_source_for_track(
        JOB,
        &release.to_ascii_uppercase(),
        &files(),
        &r,
        &winner,
        3,
        None,
    );
    assert!(prepared.is_ok());
}

#[test]
fn producer_rejects_wrong_recording_release_selection_and_duplicate_ordinals() {
    let f = files();
    let r = recordings();
    let mut wrong = selected(A, 1);
    wrong.recording_id = RECORDING_B;
    assert!(
        track_writers::original_source_for_track(JOB, RELEASE, &f, &r, &wrong, 3, None).is_err()
    );
    wrong = selected(A, 1);
    wrong.release_id = B;
    assert!(
        track_writers::original_source_for_track(JOB, RELEASE, &f, &r, &wrong, 3, None).is_err()
    );
    wrong = selected(A, 99);
    assert!(
        track_writers::original_source_for_track(JOB, RELEASE, &f, &r, &wrong, 3, None).is_err()
    );
    let mut f = files();
    f[3].ordinal = 1;
    assert!(track_writers::original_source_for_track(
        JOB,
        RELEASE,
        &f,
        &r,
        &selected(A, 1),
        3,
        None
    )
    .is_err());
}

#[test]
fn denied_or_malformed_metadata_never_loads_ciphertext() {
    let (_temp, mut store) = persisted();
    assert!(matches!(
        read_original_source(&Deny, &mut store, &Keys, A),
        Err(SourceServiceError::Denied)
    ));
    assert_eq!(store.metadata_calls, 0);
    for update in [
        "nzb_ref_encrypted=zeroblob(8388625)",
        "nzb_ref_encrypted=zeroblob(16)",
        "nzb_ref_envelope_version=1",
        "nzb_ref_payload_format=2",
        "nzb_ref_kek_version=-1",
        "nzb_ref_dek_wrapped_nonce=zeroblob(11)",
        "nzb_ref_payload_nonce=zeroblob(13)",
        "nzb_ref_dek_wrapped=zeroblob(47)",
    ] {
        store.db.execute_batch("SAVEPOINT bad").unwrap();
        store
            .db
            .execute_batch(&format!("UPDATE track_sources SET {update}"))
            .unwrap();
        let calls = store.envelope_calls;
        assert!(read_original_source(&Allow, &mut store, &Keys, A).is_err());
        assert_eq!(store.envelope_calls, calls);
        store
            .db
            .execute_batch("ROLLBACK TO bad; RELEASE bad")
            .unwrap();
    }
    store
        .db
        .execute("UPDATE track_sources SET nzb_ref_kek_version=99", [])
        .unwrap();
    assert!(matches!(
        read_original_source(&Allow, &mut store, &Keys, A),
        Err(SourceServiceError::UnknownKey)
    ));
    assert_eq!(store.envelope_calls, 0);
}

#[test]
fn cross_track_and_source_job_substitutions_fail() {
    let (_temp, mut store) = persisted();
    store
        .db
        .execute("UPDATE track_sources SET track_id=$1", [B])
        .unwrap();
    assert!(matches!(
        read_original_source(&Allow, &mut store, &Keys, B),
        Err(SourceServiceError::Crypto)
    ));
    store
        .db
        .execute("UPDATE track_sources SET track_id=$1", [A])
        .unwrap();
    store
        .db
        .execute("INSERT INTO ingest_jobs VALUES ('job_other')", [])
        .unwrap();
    store
        .db
        .execute("UPDATE track_sources SET source_job_id='job_other'", [])
        .unwrap();
    assert!(matches!(
        read_original_source(&Allow, &mut store, &Keys, A),
        Err(SourceServiceError::Invalid)
    ));
}

#[test]
fn canonical_shared_aad_is_byte_exact_and_domain_separated() {
    let aad = context(A).aad("payload", None);
    let mut expected = Vec::new();
    for s in [
        "nntune-envelope-v2",
        "payload",
        "track_sources",
        "nzb_ref",
        "shared:track-source",
        A,
    ] {
        expected.extend_from_slice(&(s.len() as u32).to_be_bytes());
        expected.extend_from_slice(s.as_bytes());
    }
    expected.extend_from_slice(&[0, 2, 0, 1]);
    assert_eq!(aad, expected);
    let source = prepare(A, 1, JOB);
    let plaintext = source.encode().unwrap();
    for different in [
        Context {
            table: "providers",
            ..context(A)
        },
        Context {
            field: "other",
            ..context(A)
        },
        Context {
            scope: "user-generated",
            ..context(A)
        },
        Context {
            row: B,
            ..context(A)
        },
        Context {
            version: 1,
            ..context(A)
        },
        Context {
            format: 2,
            ..context(A)
        },
    ] {
        let e = field_envelope::seal(&[19; 32], 7, &different, &plaintext).unwrap();
        assert!(field_envelope::open(&[19; 32], 7, &context(A), &e).is_err());
    }
    let first = seal_original_source(&Keys, 7, &source).unwrap();
    let second = seal_original_source(&Keys, 7, &source).unwrap();
    assert_ne!(
        first.envelope.dek_wrapped_nonce,
        second.envelope.dek_wrapped_nonce
    );
    assert_ne!(first.envelope.payload_nonce, second.envelope.payload_nonce);
    assert_ne!(first.envelope.ciphertext, second.envelope.ciphertext);
    assert!(field_envelope::open(&[19; 32], 8, &context(A), &first.envelope).is_err());
    assert!(seal_original_source(&Keys, 99, &source).is_err());
}

#[test]
fn authenticated_but_invalid_payload_and_tamper_are_rejected() {
    let (_temp, mut store) = persisted();
    for plaintext in [
        b"{}".as_slice(),
        b"{\"articles\":[[[[]]]]}",
        b"\xff",
        b"{\"track_id\":",
    ] {
        let envelope = field_envelope::seal(&[19; 32], 7, &context(A), plaintext).unwrap();
        store.db.execute("UPDATE track_sources SET nzb_ref_encrypted=$1, nzb_ref_dek_wrapped_nonce=$2, nzb_ref_dek_wrapped=$3, nzb_ref_payload_nonce=$4",params![envelope.ciphertext,envelope.dek_wrapped_nonce.as_slice(),envelope.dek_wrapped,envelope.payload_nonce.as_slice()]).unwrap();
        assert!(matches!(
            read_original_source(&Allow, &mut store, &Keys, A),
            Err(SourceServiceError::Invalid)
        ));
    }
    let sealed = seal_original_source(&Keys, 7, &prepare(A, 1, JOB)).unwrap();
    for position in [0, sealed.envelope.ciphertext.len() - 1] {
        let mut e = sealed.envelope.clone();
        e.ciphertext[position] ^= 1;
        assert!(field_envelope::open(&[19; 32], 7, &context(A), &e).is_err());
    }
}

#[test]
fn missing_duplicate_and_racing_projections_fail_closed() {
    struct Projection {
        rows: [Option<SourceMetadata>; 2],
        loaded: bool,
    }
    impl SourceRepository for Projection {
        fn metadata(&mut self, _: &str) -> Result<[Option<SourceMetadata>; 2], SourceServiceError> {
            Ok(self.rows.clone())
        }
        fn envelope(
            &mut self,
            _: &SourceMetadata,
        ) -> Result<Option<FieldEnvelope>, SourceServiceError> {
            self.loaded = true;
            Ok(None)
        }
    }
    let metadata = seal_original_source(&Keys, 7, &prepare(A, 1, JOB))
        .unwrap()
        .metadata;
    for rows in [
        [None, None],
        [Some(metadata.clone()), Some(metadata.clone())],
    ] {
        let mut p = Projection {
            rows,
            loaded: false,
        };
        assert!(read_original_source(&Allow, &mut p, &Keys, A).is_err());
        assert!(!p.loaded);
    }
    let mut p = Projection {
        rows: [Some(metadata), None],
        loaded: false,
    };
    assert!(read_original_source(&Allow, &mut p, &Keys, A).is_err());
    assert!(p.loaded);
}
