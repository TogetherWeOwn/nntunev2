//! Unmounted persistence of inactive sealer-origin candidate hashes.
//!
//! This seam does not authorize a namespace, publish a map, or enable reuse.
//! Callers must obtain identities from an authorized immutable intent, not from
//! a provider or an untrusted request.

use nntune_core::digest_enrollment::OriginManifest;

// Not wired until authority and custody can be established by the caller.
#[allow(dead_code)]
mod reconciliation;
#[cfg(test)]
mod reconciliation_tests;

const STORE_SQL: &str = "WITH locked_reservation AS MATERIALIZED (
    SELECT r.object_id FROM public.storage_digest_reservations AS r
    JOIN public.storage_plaintext_digest_claim_intents AS c
      ON (c.object_id, c.environment_id, c.library_id, c.plaintext_bytes, c.plaintext_sha256)
       = (r.object_id, r.environment_id, r.library_id, r.plaintext_bytes, r.plaintext_sha256)
    WHERE r.environment_id = $1::text::uuid AND r.library_id = $2::text::uuid
      AND r.object_id = $3::text::uuid AND r.plaintext_bytes = $7
      AND r.plaintext_sha256 = $11
    FOR UPDATE OF r
)
SELECT public.store_storage_digest_candidate(
    i.environment_id, i.library_id, i.object_id, $6::text::uuid, $9, $10)
FROM public.storage_object_intents AS i
JOIN locked_reservation AS r ON r.object_id = i.object_id
WHERE i.environment_id = $1::text::uuid AND i.library_id = $2::text::uuid
  AND i.object_id = $3::text::uuid AND i.generation_id = $4::text::uuid
  AND i.provider_configuration_version_id = $5::text::uuid
  AND i.plaintext_bytes = $7 AND i.header_tag = $8";

use crate::digest_candidate_identity::candidate_hashes;
pub use crate::digest_candidate_identity::{CandidateError, CandidateIntent};

fn uuid(id: &[u8; 16]) -> String {
    id.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// A single statement locks the digest-keyed reservation, matches the immutable
/// DB intent, and invokes the fenced candidate function. This lock is not an
/// authoritative reconciliation fence or proof of key custody.
///
/// Pass a dedicated, freshly connected client in autocommit mode, with no open
/// transaction and no other statements using the connection. Only under this
/// precondition does `Ok(())` confirm the candidate write committed. This
/// function does not commit a caller's explicit transaction.
///
/// The owned client is dropped on timeout, but that does not prove rollback:
/// `UnknownCommit` requires reconciliation before retry. Returned errors
/// never expose DB details.
pub async fn store(
    client: tokio_postgres::Client,
    intent: &CandidateIntent,
    manifest: &OriginManifest,
) -> Result<(), CandidateError> {
    store_with_deadline(
        client,
        intent,
        manifest,
        worker::Delay::from(std::time::Duration::from_millis(
            crate::library::LIBRARY_DB_DEADLINE_MS,
        )),
    )
    .await
}

async fn store_with_deadline<D: std::future::Future<Output = ()>>(
    client: tokio_postgres::Client,
    intent: &CandidateIntent,
    manifest: &OriginManifest,
    deadline: D,
) -> Result<(), CandidateError> {
    let hashes: Vec<Vec<u8>> = candidate_hashes(intent, manifest)?
        .iter()
        .map(|hash| hash.to_vec())
        .collect();
    let ids = intent.identities.map(|id| uuid(&id));
    let map = uuid(&intent.map_id);
    let length = i64::try_from(intent.plaintext_len).map_err(|_| CandidateError::Identity)?;
    let header = intent.header.to_vec();
    let digest = manifest.plaintext_sha256().to_vec();
    let generation = intent.reservation_generation;
    crate::library::with_db_deadline(
        async move {
            client
                .query_opt(
                    STORE_SQL,
                    &[
                        &ids[0],
                        &ids[1],
                        &ids[2],
                        &ids[3],
                        &ids[4],
                        &map,
                        &length,
                        &header,
                        &generation,
                        &hashes,
                        &digest,
                    ],
                )
                .await
                .map_err(|error| {
                    if crate::library::is_deterministic_sqlstate(error.code().map(|c| c.code())) {
                        CandidateError::Permanent
                    } else {
                        CandidateError::UnknownCommit
                    }
                })?
                .ok_or(CandidateError::MissingOrFenced)?;
            Ok(())
        },
        deadline,
        || CandidateError::UnknownCommit,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use nntune_core::digest_enrollment::DigestEnrollment;

    pub(super) fn id(value: u8) -> [u8; 16] {
        let mut id = [value; 16];
        id[6] = 0x40;
        id[8] = 0x80;
        id
    }

    pub(super) async fn connect(url: &str) -> tokio_postgres::Client {
        let (client, connection) = tokio_postgres::connect(url, tokio_postgres::NoTls)
            .await
            .unwrap();
        tokio::spawn(async move { connection.await.unwrap() });
        client
    }

    pub(super) async fn assert_no_candidate(db: &tokio_postgres::Client, object: &str, map: &str) {
        let row = db.query_one(
            "SELECT (SELECT count(*) FROM public.storage_digest_map_intents WHERE object_id = $1::text::uuid OR map_id = $2::text::uuid),\n                    (SELECT count(*) FROM public.storage_digest_map_page_intents WHERE map_id = $2::text::uuid)",
            &[&object, &map],
        ).await.unwrap();
        assert_eq!((row.get::<_, i64>(0), row.get::<_, i64>(1)), (0, 0));
    }

    // Runs only against the migrated disposable Actions database.
    #[tokio::test]
    #[ignore = "requires a migrated disposable PostgreSQL database"]
    async fn postgres_candidate_adapter_fences_and_persists() {
        let url = std::env::var("NNTUNE_TEST_DATABASE_URL").expect("scratch database URL");
        assert!(
            std::env::var("GITHUB_ACTIONS").as_deref() == Ok("true")
                && std::env::var("RUNNER_ENVIRONMENT").as_deref() == Ok("github-hosted")
                && url == "postgresql://postgres:ci-throwaway@localhost:5432/nntune_ci",
            "disposable GitHub-hosted migration database required"
        );
        let db = connect(&url).await;
        let database: String = db
            .query_one("SELECT current_database()", &[])
            .await
            .unwrap()
            .get(0);
        assert_eq!(
            database, "nntune_ci",
            "use the disposable Actions database only"
        );
        let identities = [id(21), id(22), id(23), id(24), id(25)];
        let ids = identities.map(|value| uuid(&value));
        let mut builder = DigestEnrollment::new(1, identities, id(26)).unwrap();
        let header = builder.header().to_bytes();
        builder.seal_chunk(&[42]).unwrap();
        let manifest = builder.finish().ok().unwrap();
        let mut intent = CandidateIntent {
            identities,
            plaintext_len: 1,
            map_id: id(26),
            header,
            reservation_generation: 1,
        };
        db.batch_execute(&format!(
            "INSERT INTO public.storage_environments VALUES ('{0}');
             INSERT INTO public.storage_library_namespaces VALUES ('{1}', '{0}', NULL);
             INSERT INTO public.storage_object_intents VALUES ('{2}', '{3}', '{0}', '{1}', '{4}', 1, 65, decode('{5}', 'hex'));
             INSERT INTO public.storage_plaintext_digest_claim_intents SELECT object_id, environment_id, library_id, plaintext_bytes, decode('{6}','hex') FROM public.storage_object_intents WHERE object_id = '{2}';
             SELECT public.reserve_storage_digest('{2}', 3600);",
            ids[0], ids[1], ids[2], ids[3], ids[4], hex::encode(header), hex::encode(manifest.plaintext_sha256())
        )).await.unwrap();

        // A manifest mismatch must be rejected before persistence.
        intent.plaintext_len = 2;
        assert_eq!(
            store_with_deadline(
                connect(&url).await,
                &intent,
                &manifest,
                std::future::pending()
            )
            .await,
            Err(CandidateError::Identity)
        );
        assert_no_candidate(&db, &ids[2], &uuid(&intent.map_id)).await;
        intent.plaintext_len = 1;

        // An already-fired deadline must report uncertainty, not authorize a retry.
        assert_eq!(
            store_with_deadline(
                connect(&url).await,
                &intent,
                &manifest,
                std::future::ready(())
            )
            .await,
            Err(CandidateError::UnknownCommit)
        );
        assert_no_candidate(&db, &ids[2], &uuid(&intent.map_id)).await;

        // An independently sealed manifest with the same identities must not
        // match an intent bound to a different origin header.
        let mut other_origin = DigestEnrollment::new(1, identities, id(26)).unwrap();
        let other_header = other_origin.header().to_bytes();
        assert_ne!(other_header, header);
        other_origin.seal_chunk(&[42]).unwrap();
        let other_manifest = other_origin.finish().ok().unwrap();
        let other_intent = CandidateIntent {
            header: other_header,
            ..intent
        };
        assert_eq!(
            store_with_deadline(
                connect(&url).await,
                &other_intent,
                &other_manifest,
                std::future::pending()
            )
            .await,
            Err(CandidateError::MissingOrFenced)
        );
        assert_no_candidate(&db, &ids[2], &uuid(&intent.map_id)).await;

        let mut other_ids = identities;
        other_ids[4] = id(27);
        let mut other_builder = DigestEnrollment::new(1, other_ids, id(26)).unwrap();
        let other_header = other_builder.header().to_bytes();
        other_builder.seal_chunk(&[42]).unwrap();
        let other_manifest = other_builder.finish().ok().unwrap();
        let other_intent = CandidateIntent {
            identities: other_ids,
            header: other_header,
            ..intent
        };
        assert_eq!(
            store_with_deadline(
                connect(&url).await,
                &other_intent,
                &other_manifest,
                std::future::pending()
            )
            .await,
            Err(CandidateError::MissingOrFenced)
        );
        assert_no_candidate(&db, &ids[2], &uuid(&intent.map_id)).await;

        // Match the sealed header and every DB identity except provider version.
        other_ids[2] = id(28);
        other_ids[3] = id(29);
        let other_strings = other_ids.map(|value| uuid(&value));
        let mut provider_origin = DigestEnrollment::new(1, other_ids, id(30)).unwrap();
        let provider_header = provider_origin.header().to_bytes();
        provider_origin.seal_chunk(&[43]).unwrap();
        let provider_manifest = provider_origin.finish().ok().unwrap();
        let provider_intent = CandidateIntent {
            identities: other_ids,
            map_id: id(30),
            header: provider_header,
            ..intent
        };
        db.batch_execute(&format!(
            "INSERT INTO public.storage_object_intents VALUES ('{0}', '{1}', '{2}', '{3}', '{4}', 1, 65, decode('{5}', 'hex'));
             INSERT INTO public.storage_plaintext_digest_claim_intents SELECT object_id, environment_id, library_id, plaintext_bytes, decode('{6}','hex') FROM public.storage_object_intents WHERE object_id = '{0}';",
            other_strings[2], other_strings[3], ids[0], ids[1], ids[4], hex::encode(provider_header), hex::encode(provider_manifest.plaintext_sha256())
        )).await.unwrap();
        let provider_generation: i64 = db
            .query_one(
                "SELECT public.reserve_storage_digest($1::text::uuid, 3600)",
                &[&other_strings[2]],
            )
            .await
            .unwrap()
            .get(0);
        assert_eq!(provider_generation, 1);
        assert_eq!(
            store_with_deadline(
                connect(&url).await,
                &provider_intent,
                &provider_manifest,
                std::future::pending()
            )
            .await,
            Err(CandidateError::MissingOrFenced)
        );
        assert_no_candidate(&db, &other_strings[2], &uuid(&provider_intent.map_id)).await;

        // All other identities match, but the persisted assertion is not the
        // digest produced from the sealer's plaintext.
        let mismatch_ids = [id(37), id(38), id(39), id(40), id(41)];
        let mismatch_strings = mismatch_ids.map(|value| uuid(&value));
        let mut mismatch_builder = DigestEnrollment::new(1, mismatch_ids, id(42)).unwrap();
        let mismatch_header = mismatch_builder.header().to_bytes();
        mismatch_builder.seal_chunk(&[42]).unwrap();
        let mismatch_manifest = mismatch_builder.finish().ok().unwrap();
        let mismatch_intent = CandidateIntent {
            identities: mismatch_ids,
            plaintext_len: 1,
            map_id: id(42),
            header: mismatch_header,
            reservation_generation: 1,
        };
        db.batch_execute(&format!(
            "INSERT INTO public.storage_environments VALUES ('{0}');
             INSERT INTO public.storage_library_namespaces VALUES ('{1}', '{0}', NULL);
             INSERT INTO public.storage_object_intents VALUES ('{2}', '{3}', '{0}', '{1}', '{4}', 1, 65, decode('{5}', 'hex'));
             INSERT INTO public.storage_plaintext_digest_claim_intents SELECT object_id, environment_id, library_id, plaintext_bytes, decode(repeat('ab',32),'hex') FROM public.storage_object_intents WHERE object_id = '{2}';
             SELECT public.reserve_storage_digest('{2}', 3600);",
            mismatch_strings[0], mismatch_strings[1], mismatch_strings[2], mismatch_strings[3], mismatch_strings[4], hex::encode(mismatch_header)
        )).await.unwrap();
        let mismatch_row = db.query_one(
            "SELECT plaintext_sha256, generation FROM public.storage_digest_reservations WHERE object_id = $1::text::uuid",
            &[&mismatch_strings[2]],
        ).await.unwrap();
        let asserted_digest: Vec<u8> = mismatch_row.get(0);
        assert_ne!(asserted_digest, mismatch_manifest.plaintext_sha256());
        assert_eq!(mismatch_row.get::<_, i64>(1), 1);
        assert_eq!(
            store_with_deadline(
                connect(&url).await,
                &mismatch_intent,
                &mismatch_manifest,
                std::future::pending()
            )
            .await,
            Err(CandidateError::MissingOrFenced)
        );
        assert_no_candidate(&db, &mismatch_strings[2], &uuid(&mismatch_intent.map_id)).await;

        intent.reservation_generation = 2;
        assert_eq!(
            store_with_deadline(
                connect(&url).await,
                &intent,
                &manifest,
                std::future::pending()
            )
            .await,
            Err(CandidateError::Permanent)
        );
        assert_no_candidate(&db, &ids[2], &uuid(&intent.map_id)).await;
        db.execute("UPDATE public.storage_digest_reservations SET expires_at = clock_timestamp() - interval '1 second' WHERE object_id = $1::text::uuid", &[&ids[2]]).await.unwrap();
        intent.reservation_generation = 1;
        assert_eq!(
            store_with_deadline(
                connect(&url).await,
                &intent,
                &manifest,
                std::future::pending()
            )
            .await,
            Err(CandidateError::Permanent)
        );
        assert_no_candidate(&db, &ids[2], &uuid(&intent.map_id)).await;
        let reacquired: i64 = db
            .query_one(
                "SELECT public.reserve_storage_digest($1::text::uuid, 3600)",
                &[&ids[2]],
            )
            .await
            .unwrap()
            .get(0);
        assert_eq!(reacquired, 2);
        // Reacquisition must fence the previous holder even though the lease is live again.
        assert_eq!(
            store_with_deadline(
                connect(&url).await,
                &intent,
                &manifest,
                std::future::pending()
            )
            .await,
            Err(CandidateError::Permanent)
        );
        assert_no_candidate(&db, &ids[2], &uuid(&intent.map_id)).await;
        intent.reservation_generation = 2;

        // Hold an uncommitted successor on the same digest key. The actual
        // adapter must wait on that row rather than writing beside it.
        let locker = connect(&url).await;
        locker.batch_execute("BEGIN").await.unwrap();
        locker.execute("UPDATE public.storage_digest_reservations SET expires_at = clock_timestamp() - interval '1 second' WHERE object_id = $1::text::uuid", &[&ids[2]]).await.unwrap();
        let successor: i64 = locker
            .query_one(
                "SELECT public.reserve_storage_digest($1::text::uuid, 3600)",
                &[&ids[2]],
            )
            .await
            .unwrap()
            .get(0);
        assert_eq!(successor, 3);
        let waiting_writer = connect(&url).await;
        waiting_writer
            .batch_execute("SET lock_timeout = '250ms'")
            .await
            .unwrap();
        assert_eq!(
            store_with_deadline(waiting_writer, &intent, &manifest, std::future::pending()).await,
            Err(CandidateError::UnknownCommit)
        );
        assert_no_candidate(&db, &ids[2], &uuid(&intent.map_id)).await;
        locker.batch_execute("COMMIT").await.unwrap();
        let committed = db.query_one(
            "SELECT generation, expires_at > clock_timestamp() FROM public.storage_digest_reservations WHERE environment_id = $1::text::uuid AND library_id = $2::text::uuid AND object_id = $3::text::uuid AND plaintext_bytes = $4 AND plaintext_sha256 = $5",
            &[&ids[0], &ids[1], &ids[2], &1_i64, &manifest.plaintext_sha256().to_vec()],
        ).await.unwrap();
        assert_eq!(committed.get::<_, i64>(0), successor);
        assert!(committed.get::<_, bool>(1));
        assert_eq!(
            store_with_deadline(
                connect(&url).await,
                &intent,
                &manifest,
                std::future::pending()
            )
            .await,
            Err(CandidateError::Permanent)
        );
        assert_no_candidate(&db, &ids[2], &uuid(&intent.map_id)).await;
        intent.reservation_generation = successor;
        assert_eq!(
            store_with_deadline(
                connect(&url).await,
                &intent,
                &manifest,
                std::future::pending()
            )
            .await,
            Ok(())
        );
        let manifest_row = db.query_one(
            "SELECT object_id = $2::text::uuid AND environment_id = $3::text::uuid AND library_id = $4::text::uuid, plaintext_bytes, chunk_count, page_count FROM public.storage_digest_map_intents WHERE map_id = $1::text::uuid",
            &[&uuid(&intent.map_id), &ids[2], &ids[0], &ids[1]],
        ).await.unwrap();
        assert!(manifest_row.get::<_, bool>(0));
        assert_eq!(manifest_row.get::<_, i64>(1), 1);
        assert_eq!(manifest_row.get::<_, i32>(2), 1);
        assert_eq!(manifest_row.get::<_, i32>(3), 1);
        let rows = db.query(
            "SELECT page_index, chunk_count, entry_count, expected_hash FROM public.storage_digest_map_page_intents WHERE map_id = $1::text::uuid ORDER BY page_index",
            &[&uuid(&intent.map_id)],
        ).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].get::<_, i32>(0), 0);
        assert_eq!(rows[0].get::<_, i32>(1), 1);
        assert_eq!(rows[0].get::<_, i32>(2), 1);
        assert_eq!(
            rows[0].get::<_, Vec<u8>>(3),
            candidate_hashes(&intent, &manifest).unwrap()[0]
        );

        // A duplicate is a refusal, not evidence that an ambiguous write is safe to retry.
        assert_eq!(
            store_with_deadline(
                connect(&url).await,
                &intent,
                &manifest,
                std::future::pending()
            )
            .await,
            Err(CandidateError::Permanent)
        );
        // An expired or fenced reservation also returns Permanent. Check the lease
        // after the call so that path cannot satisfy the duplicate assertion.
        let reservation_is_live: bool = db.query_one(
            "SELECT EXISTS (SELECT FROM public.storage_digest_reservations WHERE object_id = $1::text::uuid AND environment_id = $2::text::uuid AND library_id = $3::text::uuid AND generation = $4 AND expires_at > clock_timestamp())",
            &[&ids[2], &ids[0], &ids[1], &intent.reservation_generation],
        ).await.unwrap().get(0);
        assert!(
            reservation_is_live,
            "duplicate write must use a live reservation"
        );
        let persisted = db.query_one(
            "SELECT (SELECT count(*) FROM public.storage_digest_map_intents WHERE map_id = $1::text::uuid), (SELECT count(*) FROM public.storage_digest_map_page_intents WHERE map_id = $1::text::uuid), (SELECT expected_hash FROM public.storage_digest_map_page_intents WHERE map_id = $1::text::uuid AND page_index = 0)",
            &[&uuid(&intent.map_id)],
        ).await.unwrap();
        assert_eq!(persisted.get::<_, i64>(0), 1);
        assert_eq!(persisted.get::<_, i64>(1), 1);
        assert_eq!(persisted.get::<_, Vec<u8>>(2), rows[0].get::<_, Vec<u8>>(3));

        // Exercise the actual adapter across a page boundary, including the
        // partial final page, rather than only the one-page case above.
        let multi_ids = [id(31), id(32), id(33), id(34), id(35)];
        let multi_uuids = multi_ids.map(|value| uuid(&value));
        let length = 256 * 65536 + 1;
        let mut multi = DigestEnrollment::new(length, multi_ids, id(36)).unwrap();
        let multi_header = multi.header().to_bytes();
        let chunk = vec![42; 65536];
        for _ in 0..256 {
            multi.seal_chunk(&chunk).unwrap();
        }
        multi.seal_chunk(&[42]).unwrap();
        let multi_manifest = multi.finish().ok().unwrap();
        let multi_intent = CandidateIntent {
            identities: multi_ids,
            plaintext_len: length,
            map_id: id(36),
            header: multi_header,
            reservation_generation: 1,
        };
        db.batch_execute(&format!(
            "INSERT INTO public.storage_environments VALUES ('{0}');
             INSERT INTO public.storage_library_namespaces VALUES ('{1}', '{0}', NULL);
             INSERT INTO public.storage_object_intents VALUES ('{2}', '{3}', '{0}', '{1}', '{4}', {5}, {6}, decode('{7}', 'hex'));
             INSERT INTO public.storage_plaintext_digest_claim_intents SELECT object_id, environment_id, library_id, plaintext_bytes, decode('{8}','hex') FROM public.storage_object_intents WHERE object_id = '{2}';",
            multi_uuids[0], multi_uuids[1], multi_uuids[2], multi_uuids[3], multi_uuids[4],
            length, length + 48 + 257 * 16, hex::encode(multi_header), hex::encode(multi_manifest.plaintext_sha256())
        )).await.unwrap();
        let generation: i64 = db
            .query_one(
                "SELECT public.reserve_storage_digest($1::text::uuid, 3600)",
                &[&multi_uuids[2]],
            )
            .await
            .unwrap()
            .get(0);
        assert_eq!(generation, multi_intent.reservation_generation);
        assert_eq!(
            store_with_deadline(
                connect(&url).await,
                &multi_intent,
                &multi_manifest,
                std::future::pending()
            )
            .await,
            Ok(())
        );
        let multi_row = db.query_one(
            "SELECT object_id = $2::text::uuid AND environment_id = $3::text::uuid AND library_id = $4::text::uuid, plaintext_bytes, chunk_count, page_count FROM public.storage_digest_map_intents WHERE map_id = $1::text::uuid",
            &[&uuid(&multi_intent.map_id), &multi_uuids[2], &multi_uuids[0], &multi_uuids[1]],
        ).await.unwrap();
        assert!(multi_row.get::<_, bool>(0));
        assert_eq!(multi_row.get::<_, i64>(1), length as i64);
        assert_eq!(multi_row.get::<_, i32>(2), 257);
        assert_eq!(multi_row.get::<_, i32>(3), 2);
        let multi_rows = db.query(
            "SELECT page_index, chunk_count, entry_count, expected_hash FROM public.storage_digest_map_page_intents WHERE map_id = $1::text::uuid ORDER BY page_index",
            &[&uuid(&multi_intent.map_id)],
        ).await.unwrap();
        let expected = candidate_hashes(&multi_intent, &multi_manifest).unwrap();
        assert_eq!(multi_rows.len(), 2);
        assert_eq!(expected.len(), 2);
        for (index, row) in multi_rows.iter().enumerate() {
            assert_eq!(row.get::<_, i32>(0), index as i32);
            assert_eq!(row.get::<_, i32>(1), 257);
            assert_eq!(row.get::<_, i32>(2), if index == 0 { 256 } else { 1 });
            assert_eq!(row.get::<_, Vec<u8>>(3), expected[index]);
        }
        reconciliation_tests::exercise(&url, &db).await;
    }
}
