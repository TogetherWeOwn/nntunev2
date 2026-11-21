//! Rust handoff acceptance on the disposable reservation race fixture.

use super::*;
use std::{cell::Cell, time::Duration};
use tokio_postgres::NoTls;

const OBJECT: &str = "21000000-0000-4000-8000-000000000012";

async fn connect(url: &str) -> Client {
    let (client, connection) = tokio_postgres::connect(url, NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap() });
    client
        .batch_execute("SET statement_timeout = '10s'")
        .await
        .unwrap();
    client
}

async fn observed_deadline(client: &Client) -> SystemTime {
    client
        .query_one(
            "SELECT read_until FROM public.storage_validated_digest_winners WHERE object_id = $1::text::uuid",
            &[&OBJECT],
        )
        .await
        .unwrap()
        .get(0)
}

#[tokio::test]
async fn pins_require_authorization_and_live_committed_protection() {
    let url = std::env::var("NNTUNE_TEST_DATABASE_URL").expect("disposable database URL");
    assert!(
        std::env::var("GITHUB_ACTIONS").as_deref() == Ok("true")
            && std::env::var("RUNNER_ENVIRONMENT").as_deref() == Ok("github-hosted")
            && url == "postgresql://postgres:ci-throwaway@127.0.0.1:5432/nntune_ci",
        "disposable GitHub-hosted migration database required"
    );
    let mut client = connect(&url).await;
    let observer = connect(&url).await;
    // The race fixture leaves object 12 readable but detached, with a finite read_until.
    let fixture = observer
        .query_one(
            "SELECT w.*, i.generation_id, i.provider_configuration_version_id, m.map_id,
                date_trunc('second', clock_timestamp()) + interval '10 minutes' AS deadline
         FROM public.storage_validated_digest_winners w
         JOIN public.storage_object_intents i USING (object_id)
         JOIN public.storage_digest_map_intents m USING (object_id)
         WHERE w.object_id = $1::text::uuid",
            &[&OBJECT],
        )
        .await
        .unwrap();
    let id = |column: &str| fixture.get::<_, ReadUuid>(column).0;
    let object = id("object_id");
    let target = || DigestReadTarget {
        environment: id("environment_id"),
        library: id("library_id"),
        reference: object,
        object,
        generation: id("generation_id"),
        provider_configuration: id("provider_configuration_version_id"),
        root_version: id("root_version_id"),
        plaintext_sha256: fixture
            .get::<_, &[u8]>("plaintext_sha256")
            .try_into()
            .unwrap(),
        plaintext_bytes: fixture.get("plaintext_bytes"),
    };
    let requested: SystemTime = fixture.get("deadline");
    let read_with_cutoff = async |client: &mut Client, cutoff| {
        protect_digest_read(
            client,
            target(),
            requested,
            async |_| cutoff,
            SystemTime::now,
        )
        .await
    };
    let before = observed_deadline(&observer).await;
    assert!(before < requested);
    assert!(
        read_with_cutoff(&mut client, Some(requested))
            .await
            .unwrap()
            .is_none(),
        "a winner alone cannot authorize a detached reference"
    );

    let attached: bool = observer
        .query_one(
            "SELECT public.attach_storage_digest_reference(w.environment_id, w.library_id,
             w.plaintext_sha256, w.plaintext_bytes, w.object_id, i.generation_id,
             w.root_version_id, w.object_id, $2)
         FROM public.storage_validated_digest_winners w
         JOIN public.storage_object_intents i USING (object_id)
         WHERE w.object_id = $1::text::uuid",
            &[&OBJECT, &requested],
        )
        .await
        .unwrap()
        .get(0);
    assert!(attached);
    for cutoff in [None, Some(requested - Duration::from_secs(1))] {
        assert!(read_with_cutoff(&mut client, cutoff)
            .await
            .unwrap()
            .is_none());
    }
    assert_eq!(observed_deadline(&observer).await, before);

    observer.batch_execute(
        "CREATE FUNCTION public.digest_test_reject_commit() RETURNS trigger LANGUAGE plpgsql AS $$
           BEGIN RAISE EXCEPTION 'synthetic commit refusal'; END $$;
         CREATE CONSTRAINT TRIGGER digest_test_reject_commit AFTER UPDATE
         ON public.storage_validated_digest_winners DEFERRABLE INITIALLY DEFERRED
         FOR EACH ROW WHEN (NEW.object_id = '21000000-0000-4000-8000-000000000012')
         EXECUTE FUNCTION public.digest_test_reject_commit();",
    ).await.unwrap();
    assert!(read_with_cutoff(&mut client, Some(requested))
        .await
        .is_err());
    assert_eq!(observed_deadline(&observer).await, before);
    observer
        .batch_execute(
            "DROP TRIGGER digest_test_reject_commit ON public.storage_validated_digest_winners;
         DROP FUNCTION public.digest_test_reject_commit();",
        )
        .await
        .unwrap();

    let read = protect_digest_read(
        &mut client,
        target(),
        requested,
        async |candidate| {
            assert_eq!(candidate.reference, object);
            assert_eq!(candidate.object, object);
            assert_eq!(candidate.generation, id("generation_id"));
            Some(requested)
        },
        SystemTime::now,
    )
    .await
    .unwrap()
    .expect("live committed pins");
    assert_eq!(read.pins().map_id, id("map_id"));
    assert_eq!(read.pins().media_version, "media-12");
    assert_eq!(read.pins().page_versions, ["page-12"]);
    assert_eq!(read.pins().usable_until, requested);
    assert_eq!(observed_deadline(&observer).await, requested);

    let later = requested + Duration::from_secs(1);
    let checks = Cell::new(0);
    let fixed_now = SystemTime::now();
    assert!(
        protect_digest_read(
            &mut client,
            target(),
            later,
            async |_| Some(later),
            || {
                checks.set(checks.get() + 1);
                if checks.get() < 3 {
                    fixed_now
                } else {
                    later
                }
            }
        )
        .await
        .unwrap()
        .is_none(),
        "expiry on commit must not release pins"
    );
    assert_eq!(
        observed_deadline(&observer).await,
        later,
        "the hold was committed"
    );
    assert_eq!(observer.execute(
        "DELETE FROM public.storage_validated_digest_references WHERE reference_id = $1::text::uuid AND object_id = $1::text::uuid",
        &[&OBJECT],
    ).await.unwrap(), 1);
}
