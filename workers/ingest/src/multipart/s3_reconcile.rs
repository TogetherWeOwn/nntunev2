use xml::{name::OwnedName, reader::XmlEvent::*, EventReader};

use super::{encode_query_value, Digest, S3CompletionSender, Sha256, SignedRequest, SystemTime};
use crate::multipart::{s3_receipt_reader, session_completion::valid_upload_id, S3_XML_NAMESPACE};

impl S3CompletionSender {
    pub(crate) fn bucket(&self) -> &str {
        &self.bucket
    }

    pub(crate) fn configuration(&self) -> [u8; 16] {
        self.configuration
    }

    pub(crate) async fn find_incomplete(&self, key: &str, until: SystemTime) -> Result<String, ()> {
        let reply = self
            .signed_request(
                SignedRequest {
                    configuration: self.configuration,
                    bucket: &self.bucket,
                    key: "",
                    method: "GET",
                    query: format!("max-uploads=2&prefix={}&uploads=", encode_query_value(key)),
                    body: &[],
                    payload_hash: hex::encode(Sha256::digest([])),
                    operation_headers: &[],
                },
                until,
            )
            .await?;
        if reply.status != 200 {
            return Err(());
        }
        let result = Element::document(&reply.body, "ListMultipartUploadsResult")?;
        if result.field("Bucket")? != Some(self.bucket())
            || result.field("Prefix")? != Some(key)
            || result.field("IsTruncated")? != Some("false")
            || result
                .field("MaxUploads")?
                .is_some_and(|value| value != "2")
            || result.children.iter().any(|child| {
                !matches!(
                    child.name.as_str(),
                    "Bucket"
                        | "Prefix"
                        | "IsTruncated"
                        | "MaxUploads"
                        | "Upload"
                        | "KeyMarker"
                        | "UploadIdMarker"
                        | "NextKeyMarker"
                        | "NextUploadIdMarker"
                        | "Delimiter"
                )
            })
        {
            return Err(());
        }
        for field in [
            "KeyMarker",
            "UploadIdMarker",
            "NextKeyMarker",
            "NextUploadIdMarker",
            "Delimiter",
        ] {
            if result.field(field)?.is_some_and(|value| !value.is_empty()) {
                return Err(());
            }
        }
        let mut candidate = None;
        for (index, upload) in result
            .children
            .iter()
            .filter(|child| child.name == "Upload")
            .enumerate()
        {
            let returned_key = upload.field("Key")?.ok_or(())?;
            let id = upload
                .field("UploadId")?
                .filter(|id| valid_upload_id(id))
                .ok_or(())?;
            if index >= 2 || !returned_key.starts_with(key) {
                return Err(());
            }
            if returned_key == key && candidate.replace(id).is_some() {
                return Err(());
            }
        }
        candidate.map(str::to_owned).ok_or(())
    }

    pub(crate) async fn abort_incomplete(
        &self,
        key: &str,
        id: &str,
        until: SystemTime,
    ) -> Result<(), ()> {
        let reply = self
            .signed_request(
                SignedRequest {
                    configuration: self.configuration,
                    bucket: &self.bucket,
                    key,
                    method: "DELETE",
                    query: format!("uploadId={}", encode_query_value(id)),
                    body: &[],
                    payload_hash: hex::encode(Sha256::digest([])),
                    operation_headers: &[],
                },
                until,
            )
            .await?;
        match reply.status {
            204 if reply.body.is_empty() => Ok(()),
            404 => {
                let error = Element::document(&reply.body, "Error")?;
                if error.field("Code")? != Some("NoSuchUpload") {
                    return Err(());
                }
                for (field, expected) in [
                    ("BucketName", self.bucket()),
                    ("Key", key),
                    ("UploadId", id),
                ] {
                    if error.field(field)?.is_some_and(|value| value != expected) {
                        return Err(());
                    }
                }
                if let Some(resource) = error.field("Resource")? {
                    if resource != format!("/{}/{key}", self.bucket()) {
                        return Err(());
                    }
                }
                Ok(())
            }
            _ => Err(()),
        }
    }
}

// S3 listing metadata can contain nested Owner and Initiator records. Bound
// depth and read identity fields only as unique, direct, scalar children.
struct Element {
    name: String,
    value: String,
    children: Vec<Self>,
}

impl Element {
    fn document(body: &[u8], expected: &str) -> Result<Self, ()> {
        let mut reader = s3_receipt_reader(body).map_err(|_| ())?;
        loop {
            match reader.next().map_err(|_| ())? {
                StartDocument { .. } | Whitespace(_) => {}
                StartElement { name, .. }
                    if name.local_name == expected
                        && matches!(name.namespace.as_deref(), None | Some(S3_XML_NAMESPACE)) =>
                {
                    let root = Self::read(&mut reader, name, 3)?;
                    loop {
                        match reader.next().map_err(|_| ())? {
                            EndDocument => return Ok(root),
                            Whitespace(_) => {}
                            _ => return Err(()),
                        }
                    }
                }
                _ => return Err(()),
            }
        }
    }

    fn read(reader: &mut EventReader<&[u8]>, name: OwnedName, depth: u8) -> Result<Self, ()> {
        let (mut value, mut children) = (String::new(), Vec::new());
        loop {
            match reader.next().map_err(|_| ())? {
                StartElement { name: child, .. }
                    if depth > 0 && child.namespace == name.namespace =>
                {
                    children.push(Self::read(reader, child, depth - 1)?)
                }
                Characters(text) | Whitespace(text) => value.push_str(&text),
                EndElement { .. } if children.is_empty() || value.trim().is_empty() => {
                    return Ok(Self {
                        name: name.local_name,
                        value,
                        children,
                    });
                }
                _ => return Err(()),
            }
        }
    }

    fn field(&self, name: &str) -> Result<Option<&str>, ()> {
        let mut matches = self.children.iter().filter(|child| child.name == name);
        match (matches.next(), matches.next()) {
            (Some(child), None) if child.children.is_empty() => Ok(Some(&child.value)),
            (None, None) => Ok(None),
            _ => Err(()),
        }
    }
}
