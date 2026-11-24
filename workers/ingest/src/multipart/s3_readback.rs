//! Stream the exact completed versions into the authenticated inventory verifier.

use std::{io, time::SystemTime};

use nntune_core::digest_readback::{
    EnrolledEnvelope, EnrolledReadbackError, ReadbackBody, ReadbackPin, ReadbackResponse,
};
use nntune_core::s3_signing::encode_query_value;
use sha2::{Digest, Sha256};
use worker::{js_sys::Uint8Array, web_sys::ReadableStreamDefaultReader};

use super::{
    content_length, fetch_response, next_response_chunk, RequestDeadline, S3CompletionSender,
    SignedRequest,
};
use crate::multipart::readback_inventory::{
    BucketReadbackResponse, CompletedReadbackInventory, ReadbackScope, VerifiedCompletedReadback,
};

impl S3CompletionSender {
    /// Authenticate a complete inventory from the authorized session using
    /// one absolute deadline for every version and its entire response body.
    pub async fn verify_readback<'inventory, 'receipts>(
        &self,
        inventory: &'inventory CompletedReadbackInventory<'receipts>,
        envelope: EnrolledEnvelope<'_>,
        usable_until: SystemTime,
    ) -> Result<VerifiedCompletedReadback<'inventory, 'receipts>, EnrolledReadbackError> {
        let scope = ReadbackScope {
            bucket: &self.bucket,
            configuration: self.configuration,
        };
        inventory
            .verify_with_flac_seek(scope, envelope, async |bucket, pin| {
                self.open_readback(bucket, pin, usable_until)
                    .await
                    .map_err(|()| transport_error())
            })
            .await
    }

    // The inventory verifier has already checked the canonical media and page
    // keys against the authorized envelope and sealer manifest.
    async fn open_readback(
        &self,
        bucket: &str,
        pin: ReadbackPin<'_>,
        until: SystemTime,
    ) -> Result<BucketReadbackResponse<WorkerReadbackBody>, ()> {
        let version = pin.version.filter(|id| valid_version(id)).ok_or(())?;
        let (request, mut deadline) = self.prepare_request(
            SignedRequest {
                configuration: pin.configuration,
                bucket,
                key: pin.object_key,
                method: "GET",
                query: format!("versionId={}", encode_query_value(version)),
                body: &[],
                payload_hash: hex::encode(Sha256::digest([])),
                operation_headers: &[],
            },
            until,
        )?;
        let response = deadline.run(fetch_response(&request)).await?;
        let headers = response.headers();
        let version = headers.get("x-amz-version-id").map_err(|_| ())?;
        if version.as_deref().is_some_and(|id| !valid_version(id)) {
            return Err(());
        }
        let reader = response
            .body()
            .map(|stream| ReadableStreamDefaultReader::new(&stream))
            .transpose()
            .map_err(|_| ())?;
        Ok(BucketReadbackResponse {
            bucket: self.bucket.clone(),
            response: ReadbackResponse {
                configuration: self.configuration,
                object_key: pin.object_key.into(),
                version,
                status: response.status(),
                content_length: content_length(&headers)?,
                has_content_range: headers.has("content-range").map_err(|_| ())?,
                has_content_encoding: false, // fetch_response refuses content encoding.
                body: WorkerReadbackBody {
                    deadline,
                    reader,
                    pending: None,
                },
            },
        })
    }
}

fn valid_version(id: &str) -> bool {
    id != "null"
        && (1..=1024).contains(&id.len())
        && id
            .bytes()
            .all(|byte| byte.is_ascii_graphic() && byte != b',')
}

fn transport_error() -> io::Error {
    io::Error::other("S3 readback failed")
}

struct WorkerReadbackBody {
    deadline: RequestDeadline,
    reader: Option<ReadableStreamDefaultReader>,
    // Keep a view of at most one JS chunk. Copy only what the verifier asked
    // for into WASM, including its final probe for unexpected trailing bytes.
    pending: Option<Uint8Array>,
}

impl ReadbackBody for WorkerReadbackBody {
    async fn read(&mut self, dst: &mut [u8]) -> io::Result<usize> {
        if dst.is_empty() {
            return Ok(0);
        }
        self.deadline.check().map_err(|()| transport_error())?;
        while let Some(reader) = &self.reader {
            if let Some(chunk) = self.pending.take() {
                let count = (chunk.length() as usize).min(dst.len());
                chunk.subarray(0, count as u32).copy_to(&mut dst[..count]);
                if count < chunk.length() as usize {
                    self.pending = Some(chunk.subarray(count as u32, chunk.length()));
                }
                return Ok(count);
            }
            match self
                .deadline
                .run(next_response_chunk(reader))
                .await
                .map_err(|()| transport_error())?
            {
                Some(chunk) if chunk.length() != 0 => self.pending = Some(chunk),
                Some(_) => continue,
                None => self.reader = None,
            }
        }
        Ok(0)
    }
}
