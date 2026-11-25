// Reconstruct the synthetic publisher receipt already persisted in this clone.

use crate::{private_listening::PrivateListening, storage_digest_read::ReadUuid};
use nntune_core::s3_locator::{S3Binding, S3Locator};
use nntune_ingest::{
    catalogue_publication::{PublishedObject, VerifiedOccurrence},
    digest_candidate_store::CandidateIntent,
    track_writers::TrackRow,
};
use tokio_postgres::Client;

pub(super) struct PublicationFixture {
    intent: CandidateIntent,
    locator: S3Locator,
    digest: [u8; 32],
    root: [u8; 16],
    pages: [&'static str; 1],
    environment: String,
    library: String,
}

impl PublicationFixture {
    pub(super) async fn load(writer: &Client, object: &str, page_pin: &'static str) -> Self {
        let row = writer
            .query_one(include_str!("private_library_object.sql"), &[&object])
            .await
            .unwrap();
        let id = |column| row.get::<_, ReadUuid>(column).0;
        let intent = CandidateIntent {
            identities: [
                "environment_id",
                "library_id",
                "object_id",
                "generation_id",
                "provider_configuration_version_id",
            ]
            .map(id),
            plaintext_len: row.get::<_, i64>("plaintext_bytes").try_into().unwrap(),
            map_id: id("map_id"),
            header: row.get::<_, &[u8]>("header_tag").try_into().unwrap(),
            reservation_generation: 1,
        };
        let locator = S3Locator::new(
            row.get("s3_bucket"),
            row.get("s3_key"),
            S3Binding::VersionId(row.get("s3_version_id")),
        )
        .unwrap();
        let pages = [page_pin];
        assert_eq!(row.get::<_, Vec<String>>("page_versions"), pages);
        Self {
            intent,
            locator,
            pages,
            digest: row.get::<_, &[u8]>("plaintext_sha256").try_into().unwrap(),
            root: id("root_version_id"),
            environment: row.get("environment"),
            library: row.get("library"),
        }
    }

    pub(super) fn source(&self) -> PublishedObject<'_> {
        PublishedObject {
            intent: &self.intent,
            locator: &self.locator,
            plaintext_sha256: &self.digest,
            root_version_id: self.root,
            page_versions: &self.pages,
        }
    }

    pub(super) fn occurrence<'a>(
        &self,
        job: &'a str,
        issuer: &'a str,
        mbid: &'a str,
    ) -> VerifiedOccurrence<'a> {
        VerifiedOccurrence {
            job_id: job,
            owner_id: issuer,
            object_id: self.intent.identities[2],
            track: TrackRow {
                release_id: mbid,
                mbid,
                title: "Fresh synthetic track",
                medium_no: 1,
                track_no: 99,
                file_ordinal: 0,
            },
        }
    }

    pub(super) fn policy(&self, issuer: &str) -> PrivateListening {
        PrivateListening::parse(
            &serde_json::json!({
                "owner_user_id": issuer, "invite_ids": [901, 902], "expires_at": 2_000_000_000u64,
                "library_scope": {"environment_id": self.environment, "library_id": self.library}
            })
            .to_string(),
        )
        .unwrap()
    }
}
