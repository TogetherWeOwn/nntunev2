//! Scope-bound receipt from a single S3 CreateMultipartUpload response.

use xml::reader::XmlEvent::*;

use super::{
    s3_receipt_reader, session_completion::valid_upload_id, MultipartError::InitiationReceipt,
    Result, UploadTarget, S3_XML_NAMESPACE,
};

/// The provider configuration and exact target associated with this upload ID.
pub struct InitiatedUpload<'a> {
    configuration: [u8; 16],
    bucket: &'a str,
    key: &'a str,
    upload_id: String,
}

impl<'a> InitiatedUpload<'a> {
    pub fn configuration(&self) -> [u8; 16] {
        self.configuration
    }

    pub fn target(&self) -> UploadTarget<'_> {
        UploadTarget {
            bucket: self.bucket,
            key: self.key,
            upload_id: &self.upload_id,
        }
    }

    pub fn verify(
        configuration: [u8; 16],
        bucket: &'a str,
        key: &'a str,
        status: u16,
        body: &[u8],
    ) -> Result<Self> {
        if status != 200 {
            return Err(InitiationReceipt);
        }
        let mut reader = s3_receipt_reader(body).map_err(|_| InitiationReceipt)?;
        // The shared reader coalesces text across ignored comments. Require
        // receipt field values to come from uninterrupted XML text.
        if body.windows(4).any(|bytes| bytes == b"<!--") {
            return Err(InitiationReceipt);
        }
        let mut next = || -> Result<xml::reader::XmlEvent> {
            loop {
                match reader.next().map_err(|_| InitiationReceipt)? {
                    StartDocument { .. } | Whitespace(_) => {}
                    event => return Ok(event),
                }
            }
        };
        let StartElement { name: root, .. } = next()? else {
            return Err(InitiationReceipt);
        };
        if root.local_name != "InitiateMultipartUploadResult"
            || !matches!(root.namespace.as_deref(), None | Some(S3_XML_NAMESPACE))
        {
            return Err(InitiationReceipt);
        }
        let (mut saw_bucket, mut saw_key, mut upload_id) = (false, false, None);
        loop {
            let field = match next()? {
                StartElement { name, .. } if name.namespace == root.namespace => name.local_name,
                EndElement { .. } => break,
                _ => return Err(InitiationReceipt),
            };
            let Characters(value) = next()? else {
                return Err(InitiationReceipt);
            };
            if !matches!(next()?, EndElement { .. }) {
                return Err(InitiationReceipt);
            }
            match field.as_str() {
                "Bucket" if !saw_bucket && value == bucket => saw_bucket = true,
                "Key" if !saw_key && value == key => saw_key = true,
                "UploadId" if upload_id.is_none() && valid_upload_id(&value) => {
                    upload_id = Some(value)
                }
                _ => return Err(InitiationReceipt),
            }
        }
        if !saw_bucket || !saw_key || !matches!(next()?, EndDocument) {
            return Err(InitiationReceipt);
        }
        Ok(Self {
            configuration,
            bucket,
            key,
            upload_id: upload_id.ok_or(InitiationReceipt)?,
        })
    }
}

#[cfg(test)]
#[test]
fn initiation_does_not_join_comment_split_upload_ids() {
    const BUCKET: &str = "test-bucket";
    const KEY: &str = "test-key";
    let verify = |value| {
        let xml = format!(
            "<InitiateMultipartUploadResult><Bucket>{BUCKET}</Bucket><Key>{KEY}</Key>\
             <UploadId>{value}</UploadId></InitiateMultipartUploadResult>"
        );
        InitiatedUpload::verify([5; 16], BUCKET, KEY, 200, xml.as_bytes())
    };
    assert_eq!(
        verify("opaque+&amp;=").unwrap().target().upload_id,
        "opaque+&="
    );
    assert_eq!(
        verify("opaque<!-- split -->+&amp;=").err(),
        Some(InitiationReceipt)
    );
}
