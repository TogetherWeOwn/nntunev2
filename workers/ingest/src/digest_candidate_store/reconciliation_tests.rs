//! Disposable PostgreSQL only. The harness chooses commit or rollback; the
//! observer sees neither writer reply. This does not inject a wire-level loss.

use std::future::{pending, ready};
use tokio_postgres::Client;

use super::reconciliation::{observe_with_deadline, DatabaseOutcome::*};
use super::tests::{assert_no_candidate, connect, id};
use super::{
    candidate_hashes, store_with_deadline, uuid, CandidateError, CandidateIntent, STORE_SQL,
};
use nntune_core::digest_enrollment::{DigestEnrollment, OriginManifest};

fn build(base: u8, scope: u8) -> (CandidateIntent, OriginManifest) {
    let identities = [scope, scope + 1, base + 2, base + 3, base + 4].map(id);
    let map_id = id(base + 5);
    let mut builder = DigestEnrollment::new(1, identities, map_id).unwrap();
    let header = builder.header().to_bytes();
    builder.seal_chunk(&[42]).unwrap();
    let intent = CandidateIntent {
        identities,
        plaintext_len: 1,
        map_id,
        header,
        reservation_generation: 1,
    };
    (intent, builder.finish().ok().unwrap())
}

async fn seed(db: &Client, i: &CandidateIntent, m: &OriginManifest) {
    let s = i.identities.map(|v| uuid(&v));
    db.batch_execute(&format!(
        "INSERT INTO public.storage_environments VALUES ('{0}') ON CONFLICT DO NOTHING;
         INSERT INTO public.storage_library_namespaces VALUES ('{1}', '{0}', NULL) ON CONFLICT DO NOTHING;
         INSERT INTO public.storage_object_intents VALUES ('{2}', '{3}', '{0}', '{1}', '{4}', 1, 65, decode('{5}', 'hex'));
         INSERT INTO public.storage_plaintext_digest_claim_intents SELECT object_id, environment_id, library_id, plaintext_bytes, decode('{6}','hex') FROM public.storage_object_intents WHERE object_id = '{2}';",
        s[0], s[1], s[2], s[3], s[4], hex::encode(i.header), hex::encode(m.plaintext_sha256())
    )).await.unwrap();
}

async fn expire(db: &Client, i: &CandidateIntent) {
    db.execute("UPDATE public.storage_digest_reservations SET expires_at = clock_timestamp() - interval '1 second' WHERE object_id = $1::text::uuid", &[&uuid(&i.identities[2])]).await.unwrap();
}

async fn reserve(db: &Client, i: &CandidateIntent) -> i64 {
    db.query_one(
        "SELECT public.reserve_storage_digest($1::text::uuid, 3600)",
        &[&uuid(&i.identities[2])],
    )
    .await
    .unwrap()
    .get(0)
}

async fn pid(db: &Client) -> i32 {
    db.query_one("SELECT pg_backend_pid()", &[])
        .await
        .unwrap()
        .get(0)
}

async fn wait_for_lock(db: &Client, waiter: i32, writer: i32) {
    for _ in 0..100 {
        let blocked: bool = db
            .query_one("SELECT $1 = ANY(pg_blocking_pids($2))", &[&writer, &waiter])
            .await
            .unwrap()
            .get(0);
        if blocked {
            return;
        }
        db.batch_execute("SELECT pg_sleep(0.05)").await.unwrap();
    }
    panic!("observer never blocked on the writer row lock");
}

async fn insert_map(db: &Client, map: &str, object: &str) {
    let rows = db.execute("INSERT INTO public.storage_digest_map_intents (map_id, object_id, environment_id, library_id, plaintext_bytes) SELECT $1::text::uuid, object_id, environment_id, library_id, plaintext_bytes FROM public.storage_object_intents WHERE object_id = $2::text::uuid", &[&map, &object]).await.unwrap();
    assert_eq!(rows, 1);
}

pub(super) async fn exercise(url: &str, db: &Client) {
    for (base, finish, expected) in [
        (50, "COMMIT", MatchingCandidate),
        (60, "ROLLBACK", Indeterminate),
        (80, "ROLLBACK", Indeterminate),
    ] {
        let (i, m) = build(base, base);
        let (successor, successor_manifest) = build(base + 100, base);
        seed(db, &i, &m).await;
        seed(db, &successor, &successor_manifest).await;
        assert_eq!(reserve(db, &i).await, 1);
        let [e, l, object, sealing_generation, provider] = i.identities.map(|v| uuid(&v));
        let map = uuid(&i.map_id);
        let hashes: Vec<Vec<u8>> = candidate_hashes(&i, &m)
            .unwrap()
            .iter()
            .map(|h| h.to_vec())
            .collect();
        let writer = connect(url).await;
        writer.batch_execute("BEGIN").await.unwrap();
        // Execute the adapter's exact statement but hold transaction completion
        // so the observer can be forced to take its lock snapshot first.
        writer
            .query_one(
                STORE_SQL,
                &[
                    &e,
                    &l,
                    &object,
                    &sealing_generation,
                    &provider,
                    &map,
                    &1_i64,
                    &i.header.as_slice(),
                    &i.reservation_generation,
                    &hashes,
                    &m.plaintext_sha256().as_slice(),
                ],
            )
            .await
            .unwrap();
        assert_no_candidate(db, &object, &map).await;
        let timed = connect(url).await;
        timed
            .batch_execute("SET lock_timeout = '250ms'")
            .await
            .unwrap();
        assert_eq!(
            observe_with_deadline(timed, &i, &m, pending()).await,
            Indeterminate
        );
        let observer = connect(url).await;
        let (waiter, locker) = (pid(&observer).await, pid(&writer).await);
        let (outcome, ()) =
            tokio::join!(observe_with_deadline(observer, &i, &m, pending()), async {
                wait_for_lock(db, waiter, locker).await;
                writer.batch_execute(finish).await.unwrap();
            });
        assert_eq!(outcome, expected);

        let future = CandidateIntent {
            reservation_generation: 9,
            ..i
        };
        assert_eq!(
            observe_with_deadline(connect(url).await, &future, &m, pending()).await,
            Indeterminate
        );
        assert_eq!(
            observe_with_deadline(connect(url).await, &i, &m, ready(())).await,
            Indeterminate
        );
        if finish == "COMMIT" {
            let (other, other_manifest) = build(base, base);
            assert_ne!(other.header, i.header);
            assert_eq!(
                observe_with_deadline(connect(url).await, &other, &other_manifest, pending()).await,
                Indeterminate
            );
        } else {
            assert_no_candidate(db, &object, &map).await;
        }
        expire(db, &i).await;
        assert_eq!(
            observe_with_deadline(connect(url).await, &i, &m, pending()).await,
            Indeterminate
        );
        assert_eq!(reserve(db, &successor).await, 2);
        assert_eq!(
            observe_with_deadline(connect(url).await, &i, &m, pending()).await,
            if finish == "COMMIT" {
                Indeterminate
            } else {
                FencedAbsent
            }
        );
        if finish == "ROLLBACK" {
            assert_eq!(
                store_with_deadline(connect(url).await, &i, &m, pending()).await,
                Err(CandidateError::MissingOrFenced)
            );
            assert_no_candidate(db, &object, &map).await;
            // Isolate the two join arms in separate append-only scopes.
            let (conflict_map, conflict_object) = if base == 60 {
                (uuid(&successor.map_id), object)
            } else {
                (map, uuid(&successor.identities[2]))
            };
            insert_map(db, &conflict_map, &conflict_object).await;
            assert_eq!(
                observe_with_deadline(connect(url).await, &i, &m, pending()).await,
                Indeterminate
            );
        }
    }

    let (i, m) = build(70, 70);
    seed(db, &i, &m).await;
    assert_eq!(reserve(db, &i).await, 1);
    let object = uuid(&i.identities[2]);
    let map = uuid(&i.map_id);
    insert_map(db, &map, &object).await;
    assert_eq!(
        observe_with_deadline(connect(url).await, &i, &m, pending()).await,
        Indeterminate
    );
    let mut wrong_hash = candidate_hashes(&i, &m).unwrap()[0].to_vec();
    wrong_hash[0] ^= 1;
    db.execute("INSERT INTO public.storage_digest_map_page_intents (map_id, chunk_count, page_index, expected_hash) VALUES ($1::text::uuid, 1, 0, $2)", &[&map, &wrong_hash]).await.unwrap();
    assert_eq!(
        observe_with_deadline(connect(url).await, &i, &m, pending()).await,
        Indeterminate
    );
}
