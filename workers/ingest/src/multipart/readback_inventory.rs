//! Assemble exact completion pins for bounded readback before opening any objects.

use super::{sidecar::CompletedSidecar, CompletedVersionedMedia};
use nntune_core::{
    digest_enrollment::OriginManifest, digest_geometry::DigestGeometry,
    digest_readback::ReadbackPin, media_envelope::EnvelopeContext, s3_locator::S3Binding,
};

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
    geometry: DigestGeometry,
    media: ReadbackPin<'a>,
    pages: Vec<(u32, ReadbackPin<'a>)>,
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
        manifest: &OriginManifest,
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
            geometry,
            media,
            pages,
        })
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
