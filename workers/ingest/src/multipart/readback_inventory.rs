//! Assemble and verify exact completion pins at an unmounted readback boundary.

use super::{sidecar::CompletedSidecar, CompletedVersionedMedia};
use nntune_core::{
    digest_enrollment::OriginManifest,
    digest_geometry::DigestGeometry,
    digest_readback::{
        verify_enrolled_readback, BodyReadbackError, CompletedInventory, EnrolledEnvelope,
        EnrolledReadbackError, ReadbackBody, ReadbackPin, ReadbackResponse,
    },
    media_envelope::EnvelopeContext,
    s3_locator::S3Binding,
};
use std::ops::AsyncFnMut;

#[derive(Debug, PartialEq, Eq)]
pub enum InventoryError {
    Origin,
    Media,
    Sidecars,
}

/// Scoped, ordered pins and exact lengths for readback. The completion
/// receipts supply canonical logical keys relative to the configuration and bucket.
pub struct CompletedReadbackInventory<'a> {
    bucket: &'a str,
    identities: [[u8; 16]; 5],
    map_id: [u8; 16],
    origin: &'a OriginManifest,
    geometry: DigestGeometry,
    media: ReadbackPin<'a>,
    pages: Vec<(u32, ReadbackPin<'a>)>,
}

/// Custody of the configured reader, established independently of the receipts.
pub struct ReadbackScope<'a> {
    pub bucket: &'a str,
    pub configuration: [u8; 16],
}

pub struct BucketReadbackResponse<B> {
    /// Bucket actually used for the request, established by the opener.
    pub bucket: String,
    pub response: ReadbackResponse<B>,
}

/// In-memory evidence for the exact inventory and sealer origin read to EOF.
/// Publication and reuse require separate authorization and lifecycle fencing.
pub struct VerifiedCompletedReadback<'inventory, 'receipts> {
    inventory: &'inventory CompletedReadbackInventory<'receipts>,
}

impl<'inventory, 'receipts> VerifiedCompletedReadback<'inventory, 'receipts> {
    pub fn inventory(&self) -> &'inventory CompletedReadbackInventory<'receipts> {
        self.inventory
    }

    pub fn origin(&self) -> &'receipts OriginManifest {
        self.inventory.origin
    }
}

impl<'a> CompletedReadbackInventory<'a> {
    /// Use the bucket, configuration, map ID and context from trusted upload
    /// intent. The inventory binds upload receipts to the sealer manifest;
    /// readback still has to authenticate the stored bytes and validate media.
    pub fn assemble(
        bucket: &'a str,
        configuration: [u8; 16],
        context: &EnvelopeContext,
        map_id: [u8; 16],
        manifest: &'a OriginManifest,
        media: &'a CompletedVersionedMedia,
        sidecars: &'a [CompletedSidecar<'_>],
    ) -> Result<Self, InventoryError> {
        let [_, identities @ ..] = context.identities;
        if configuration != identities[4] {
            return Err(InventoryError::Origin);
        }
        let hashes = manifest
            .candidate_page_hashes(&identities, context.plaintext_len, &map_id, &context.header)
            .ok_or(InventoryError::Origin)?;
        let geometry = DigestGeometry::new(context.plaintext_len).ok_or(InventoryError::Origin)?;

        let metadata = media.metadata();
        let locator = metadata.locator();
        let S3Binding::VersionId(version) = locator.binding() else {
            return Err(InventoryError::Media);
        };
        if locator.bucket() != bucket
            || !metadata.matches_context(context)
            || metadata.ciphertext_len() != geometry.sealed_len()
        {
            return Err(InventoryError::Media);
        }
        let media = ReadbackPin {
            configuration,
            object_key: locator.key(),
            version: Some(version),
        };

        if sidecars.len() != hashes.len() {
            return Err(InventoryError::Sidecars);
        }
        let mut pages = Vec::with_capacity(sidecars.len());
        for sidecar in sidecars {
            let (target, version) = sidecar.pin();
            // The enrolled context is stable across authorized root rewraps.
            let [_, target_identities @ ..] = target.context.identities;
            let page = geometry
                .page(target.index)
                .ok_or(InventoryError::Sidecars)?;
            if target.bucket != bucket
                || target_identities != identities
                || target.context.header != context.header
                || target.context.plaintext_len != context.plaintext_len
                || target.map_id != map_id
                || !sidecar.matches_origin(page.encoded_len, &hashes[page.index as usize])
            {
                return Err(InventoryError::Sidecars);
            }
            pages.push((
                page.index,
                ReadbackPin {
                    configuration,
                    object_key: &target.key,
                    version: Some(version),
                },
            ));
        }
        pages.sort_unstable_by_key(|&(index, _)| index);
        if pages
            .iter()
            .map(|&(index, _)| index)
            .ne(0..geometry.pages())
        {
            return Err(InventoryError::Sidecars);
        }
        Ok(Self {
            bucket,
            identities,
            map_id,
            origin: manifest,
            geometry,
            media,
            pages,
        })
    }

    /// Read the receipt versions through an independently configured opener.
    /// The opener must authenticate the request, use the exact bucket and pin,
    /// apply deadlines, reject ambiguous metadata, and avoid redirects or decoding.
    /// Report the actual request target and observed response metadata.
    pub async fn verify<'inventory, B: ReadbackBody>(
        &'inventory self,
        scope: ReadbackScope<'_>,
        envelope: EnrolledEnvelope<'_>,
        mut open: impl AsyncFnMut(
            &'a str,
            ReadbackPin<'a>,
        ) -> std::io::Result<BucketReadbackResponse<B>>,
    ) -> Result<VerifiedCompletedReadback<'inventory, 'a>, EnrolledReadbackError> {
        let inventory_error = || EnrolledReadbackError::Body(BodyReadbackError::Inventory);
        if scope.bucket != self.bucket || scope.configuration != self.media.configuration {
            return Err(inventory_error());
        }
        let media_version = self.media.version.ok_or_else(inventory_error)?;
        let page_versions = self
            .pages
            .iter()
            .map(|(_, pin)| pin.version)
            .collect::<Option<Vec<_>>>()
            .ok_or_else(inventory_error)?;
        let completed = CompletedInventory {
            identities: self.identities,
            map_id: self.map_id,
            plaintext_len: self.geometry.plaintext_len(),
            media_version,
            page_versions: &page_versions,
        };
        verify_enrolled_readback(
            self.origin,
            envelope,
            &completed,
            self.media,
            &self.pages,
            async |pin| {
                let opened = open(self.bucket, pin)
                    .await
                    .map_err(|_| BodyReadbackError::Transport)?;
                if opened.bucket != self.bucket {
                    return Err(BodyReadbackError::Response);
                }
                Ok(opened.response)
            },
        )
        .await?;
        Ok(VerifiedCompletedReadback { inventory: self })
    }

    pub fn bucket(&self) -> &str {
        self.bucket
    }

    pub fn geometry(&self) -> DigestGeometry {
        self.geometry
    }

    pub fn media(&self) -> ReadbackPin<'a> {
        self.media
    }

    pub fn pages(&self) -> &[(u32, ReadbackPin<'a>)] {
        &self.pages
    }
}
