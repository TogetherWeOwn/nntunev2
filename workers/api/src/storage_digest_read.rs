//! Unmounted, protected lookup for a previously authorized playback reference.

use nntune_core::{digest_geometry::DigestGeometry, digest_read_snapshot::DigestReadSnapshot};
use std::time::SystemTime;
use tokio_postgres::types::{FromSql, Type};

use crate::{
    handlers::StoreError,
    postgres::{PgConnect, PgQuery},
};

pub mod custody;

#[derive(PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DigestReadTarget {
    pub environment: [u8; 16],
    pub library: [u8; 16],
    pub reference: [u8; 16],
    pub object: [u8; 16],
    pub generation: [u8; 16],
    pub provider_configuration: [u8; 16],
    pub root_version: [u8; 16],
    pub plaintext_sha256: [u8; 32],
    pub plaintext_bytes: i64,
}

pub struct DigestReadPins {
    pub map_id: [u8; 16],
    pub media_version: String,
    pub page_versions: Vec<String>,
    pub usable_until: SystemTime,
}

/// The target and pins cannot be replaced or constructed outside this store.
pub struct CommittedDigestRead {
    target: DigestReadTarget,
    pins: DigestReadPins,
}

impl CommittedDigestRead {
    pub fn target(&self) -> &DigestReadTarget {
        &self.target
    }

    pub fn pins(&self) -> &DigestReadPins {
        &self.pins
    }

    /// Borrow the pins for playback; the consumer still checks the deadline.
    pub fn playback_snapshot(&self) -> Result<DigestReadSnapshot<'_>, StoreError> {
        Ok(DigestReadSnapshot {
            environment_id: self.target.environment,
            library_id: self.target.library,
            reference_id: self.target.reference,
            object_id: self.target.object,
            generation_id: self.target.generation,
            provider_configuration: self.target.provider_configuration,
            root_version_id: self.target.root_version,
            plaintext_len: u64::try_from(self.target.plaintext_bytes).map_err(|_| StoreError)?,
            plaintext_sha256: self.target.plaintext_sha256,
            map_id: self.pins.map_id,
            media_version: &self.pins.media_version,
            page_versions: &self.pins.page_versions,
            usable_until: self.pins.usable_until,
        })
    }
}

fn is_uuid_v4(id: [u8; 16]) -> bool {
    id[6] >> 4 == 4 && id[8] >> 6 == 2
}

impl DigestReadTarget {
    fn geometry(&self) -> Option<DigestGeometry> {
        [
            self.environment,
            self.library,
            self.object,
            self.generation,
            self.provider_configuration,
            self.root_version,
        ]
        .into_iter()
        .all(is_uuid_v4)
        .then_some(())?;
        let len = u64::try_from(self.plaintext_bytes).ok()?;
        (len > 0).then_some(())?;
        DigestGeometry::new(len)
    }
}

/// `authorize` is the trusted caller's independent grant/reference check, not
/// a winner lookup or an input supplied by a client. It returns the exact
/// target's authorization cutoff, or `None`. Use a fresh clock on each call;
/// returned pins cease to be usable at `usable_until`.
pub async fn protect_digest_read(
    client: &mut impl PgConnect,
    target: DigestReadTarget,
    requested_until: SystemTime,
    authorize: impl AsyncFnOnce(&DigestReadTarget) -> Option<SystemTime>,
    now: impl Fn() -> SystemTime,
) -> Result<Option<CommittedDigestRead>, StoreError> {
    let Some(geometry) = target.geometry() else {
        return Ok(None);
    };
    if requested_until <= now() {
        return Ok(None);
    }
    let Some(cutoff) = authorize(&target).await else {
        return Ok(None);
    };
    if !deadline_allowed(requested_until, cutoff, now()) {
        return Ok(None);
    }

    let tx = client.read_committed().await.map_err(|_| StoreError)?;
    let rows = tx
        .query_typed(
            include_str!("storage_digest_read_lookup.sql"),
            &[
                (&uuid_text(target.environment), Type::TEXT),
                (&uuid_text(target.library), Type::TEXT),
                (&&target.plaintext_sha256[..], Type::BYTEA),
                (&target.plaintext_bytes, Type::INT8),
                (&uuid_text(target.object), Type::TEXT),
                (&uuid_text(target.generation), Type::TEXT),
                (&uuid_text(target.root_version), Type::TEXT),
                (&uuid_text(target.reference), Type::TEXT),
                (&cutoff, Type::TIMESTAMPTZ),
                (&uuid_text(target.provider_configuration), Type::TEXT),
                (&requested_until, Type::TIMESTAMPTZ),
            ],
        )
        .await
        .map_err(|_| StoreError)?;
    let row = match rows.as_slice() {
        [] => return Ok(None),
        [row] => row,
        _ => return Err(StoreError),
    };
    let id = |column: &str| {
        row.try_get::<_, ReadUuid>(column)
            .map(|uuid| uuid.0)
            .map_err(|_| StoreError)
    };
    for (column, expected) in [
        ("object_id", target.object),
        ("generation_id", target.generation),
        (
            "provider_configuration_version_id",
            target.provider_configuration,
        ),
        ("root_version_id", target.root_version),
    ] {
        if id(column)? != expected {
            return Err(StoreError);
        }
    }
    let pins = DigestReadPins {
        map_id: id("map_id")?,
        media_version: row.try_get("media_version").map_err(|_| StoreError)?,
        page_versions: row.try_get("page_versions").map_err(|_| StoreError)?,
        usable_until: row.try_get("usable_until").map_err(|_| StoreError)?,
    };
    if !is_uuid_v4(pins.map_id)
        || pins.page_versions.len() != geometry.pages() as usize
        || !valid_version(&pins.media_version)
        || !pins
            .page_versions
            .iter()
            .all(|version| valid_version(version))
    {
        return Err(StoreError);
    }
    if !deadline_allowed(pins.usable_until, requested_until, now()) {
        return Ok(None);
    }
    tx.commit().await.map_err(|_| StoreError)?;
    Ok((now() < pins.usable_until).then_some(CommittedDigestRead { target, pins }))
}

pub(crate) fn valid_version(version: &str) -> bool {
    (1..=1024).contains(&version.len())
        && version != "null"
        && version.bytes().all(|b| b.is_ascii_graphic() && b != b',')
}

fn deadline_allowed(until: SystemTime, cutoff: SystemTime, now: SystemTime) -> bool {
    now < until && until <= cutoff
}

pub(crate) fn uuid_text(id: [u8; 16]) -> String {
    format!("{:032x}", u128::from_be_bytes(id))
}

pub(crate) struct ReadUuid(pub(crate) [u8; 16]);

impl<'a> FromSql<'a> for ReadUuid {
    fn from_sql(_: &Type, raw: &'a [u8]) -> Result<Self, Box<dyn std::error::Error + Sync + Send>> {
        Ok(Self(raw.try_into()?))
    }

    fn accepts(ty: &Type) -> bool {
        *ty == Type::UUID
    }
}

#[cfg(all(test, feature = "postgres-tests", not(target_arch = "wasm32")))]
#[path = "storage_digest_read_postgres.rs"]
mod postgres_tests;

#[cfg(all(test, not(target_arch = "wasm32")))]
#[path = "../tests/support/private_playback_offline_client.rs"]
mod offline;
#[cfg(all(test, not(target_arch = "wasm32")))]
use crate::postgres::PgClient;

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, UNIX_EPOCH};

    #[cfg(not(target_arch = "wasm32"))]
    fn target() -> DigestReadTarget {
        let id = 0x2100_0000_0000_4000_8000_0000_0000_00ffu128.to_be_bytes();
        DigestReadTarget {
            environment: id,
            library: id,
            reference: id,
            object: id,
            generation: id,
            provider_configuration: id,
            root_version: id,
            plaintext_sha256: [42; 32],
            plaintext_bytes: 1,
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[tokio::test]
    async fn custody_rejects_another_objects_locator_before_reading() {
        use custody::{resolve_custody, PlaybackRoot};
        use nntune_core::s3_locator::{S3Binding, S3Locator};

        let mut db = offline::offline_client().await;
        let now = UNIX_EPOCH;
        let root = PlaybackRoot {
            version: target().root_version,
            secret: &[42; 32],
        };
        for (object, should_query) in [
            (target().object, true),
            (target().object.map(|b| b ^ 1), false),
        ] {
            let target = target();
            let generation = uuid_text(target.generation);
            let object = uuid_text(object);
            let locator = S3Locator::new(
                "fixture-bucket".into(),
                format!("media/{object}/{generation}"),
                S3Binding::VersionId("media-version".into()),
            )
            .unwrap();
            let read = CommittedDigestRead {
                target,
                pins: DigestReadPins {
                    map_id: root.version,
                    media_version: "media-version".into(),
                    page_versions: vec!["page-version".into()],
                    usable_until: now + Duration::from_secs(1),
                },
            };
            let result = resolve_custody(&mut db, read, &locator, &root, || now).await;
            assert!(match result {
                Err(_) => should_query,
                Ok(None) => !should_query,
                Ok(Some(_)) => false,
            });
        }
    }

    #[test]
    fn deadline_requires_future_inclusive_authorization_cutoff() {
        let now = UNIX_EPOCH;
        let expiry = now + Duration::from_secs(1);
        assert!(deadline_allowed(expiry, expiry, now));
        assert!(!deadline_allowed(now, expiry, now));
        assert!(!deadline_allowed(expiry, now, now));
    }

    #[test]
    fn uuid_codec_keeps_all_bytes_in_wire_order() {
        let bytes = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15];
        assert_eq!(uuid_text(bytes), "000102030405060708090a0b0c0d0e0f");
        assert_eq!(ReadUuid::from_sql(&Type::UUID, &bytes).unwrap().0, bytes);
        assert!(ReadUuid::from_sql(&Type::UUID, &bytes[..15]).is_err());
    }
}
