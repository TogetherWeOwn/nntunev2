//! Bounded internal account/resolver replies. No routing or dialing authority.
//! The caller retains the whole-call deadline; dropping this exchange must
//! cancel both a pending binding fetch and any pending response-body read.

use nntune_core::pool_channel::{MAX_OPERATION_BYTES, MAX_REQUEST_BYTES};
use zeroize::Zeroizing;

use crate::{
    account_operation::MAX_DIAL_BYTES, account_route::RegistryError,
    account_route::MAX_REGISTRY_BYTES,
};

#[cfg(target_arch = "wasm32")]
pub(crate) mod worker;

#[derive(Clone, Copy)]
pub(crate) enum Kind {
    Registry,
    Resolve,
    Candidate,
    Operation,
}

impl Kind {
    fn request_limit(self) -> usize {
        match self {
            Self::Operation => MAX_OPERATION_BYTES,
            _ => MAX_REQUEST_BYTES,
        }
    }

    fn response_limit(self, content_type: &str) -> Result<usize, RegistryError> {
        match (self, content_type) {
            (Self::Registry, "application/json" | "application/json; charset=utf-8") => {
                Ok(MAX_REGISTRY_BYTES)
            }
            (Self::Resolve, "application/json" | "application/json; charset=utf-8") => {
                Ok(MAX_DIAL_BYTES)
            }
            (Self::Candidate, "application/json") => Ok(MAX_REQUEST_BYTES),
            (Self::Operation, "application/json") => Ok(MAX_OPERATION_BYTES),
            (Self::Operation, "application/octet-stream") => {
                Ok(crate::body::MAX_BODY_ARTICLE_BYTES)
            }
            _ => Err(RegistryError),
        }
    }
}

pub(crate) struct Head {
    pub status: u16,
    pub content_type: String,
    pub provider_id: Option<String>,
    pub cache_control: String,
    pub content_length: Option<String>,
    pub content_encoding: Option<String>,
}

impl Head {
    fn validate(&self, kind: Kind) -> Result<(usize, Option<usize>), RegistryError> {
        let cap = kind.response_limit(&self.content_type)?;
        // The API renderer and resolver each append no-store. Admit only
        // that exact repeated directive on the two API reply kinds.
        let cache_ok = self.cache_control.eq_ignore_ascii_case("no-store")
            || (matches!(kind, Kind::Registry | Kind::Resolve)
                && self
                    .cache_control
                    .eq_ignore_ascii_case("no-store, no-store"));
        if !cache_ok
            || self
                .content_encoding
                .as_deref()
                .is_some_and(|value| !value.eq_ignore_ascii_case("identity"))
            || match kind {
                Kind::Operation => !self
                    .provider_id
                    .as_deref()
                    .is_some_and(nntune_core::pool_operation::valid_provider_id),
                _ => self.provider_id.is_some(),
            }
        {
            return Err(RegistryError);
        }
        let length = self
            .content_length
            .as_deref()
            .map(|value| {
                if value.is_empty()
                    || value.len() > 10
                    || !value.bytes().all(|b| b.is_ascii_digit())
                {
                    return Err(RegistryError);
                }
                value
                    .parse::<usize>()
                    .ok()
                    .filter(|n| *n <= cap)
                    .ok_or(RegistryError)
            })
            .transpose()?;
        Ok((cap, length))
    }
}

pub(crate) struct Response {
    pub head: Head,
    pub bytes: Zeroizing<Vec<u8>>,
}

#[allow(async_fn_in_trait)]
pub(crate) trait Client {
    type Reply: Reply;
    /// The future owns abort authority before its first suspension and transfers
    /// it into Reply on success. No detached read/fetch tasks.
    async fn post(&self, authorization: &str, body: &[u8]) -> Result<Self::Reply, RegistryError>;
}

#[allow(async_fn_in_trait)]
pub(crate) trait Reply {
    fn head(&self) -> &Head;
    /// Inspect raw length before copying into Wasm; zeroize resolver chunks.
    async fn next(&mut self, remaining: usize)
        -> Result<Option<Zeroizing<Vec<u8>>>, RegistryError>;
}

pub(crate) async fn exchange(
    client: &impl Client,
    kind: Kind,
    authorization: &str,
    body: &[u8],
) -> Result<Response, RegistryError> {
    if authorization.is_empty() || authorization.len() > 256 || body.len() > kind.request_limit() {
        return Err(RegistryError);
    }
    let mut reply = client.post(authorization, body).await?;
    let head = reply.head();
    if head.status != 200 {
        // Preserve status mapping without reading, copying or exposing error text.
        return Ok(Response {
            head: Head {
                status: head.status,
                content_type: String::new(),
                provider_id: None,
                cache_control: String::new(),
                content_length: None,
                content_encoding: None,
            },
            bytes: Zeroizing::new(Vec::new()),
        });
    }
    let (cap, length) = head.validate(kind)?;
    let mut bytes = Zeroizing::new(Vec::new());
    loop {
        let Some(chunk) = reply.next(cap - bytes.len()).await? else {
            break;
        };
        // Defense in depth for implementations that violate the pre-copy bound.
        if chunk.is_empty() || chunk.len() > cap - bytes.len() {
            return Err(RegistryError);
        }
        bytes.extend_from_slice(&chunk);
    }
    if length.is_some_and(|n| n != bytes.len()) {
        return Err(RegistryError);
    }
    let head = reply.head();
    Ok(Response {
        head: Head {
            status: head.status,
            content_type: head.content_type.clone(),
            provider_id: head.provider_id.clone(),
            cache_control: head.cache_control.clone(),
            content_length: head.content_length.clone(),
            content_encoding: head.content_encoding.clone(),
        },
        bytes,
    })
}

fn copy_header(
    length: u32,
    limit: u32,
    copy: impl FnOnce() -> Option<String>,
) -> Result<String, RegistryError> {
    if length > limit {
        return Err(RegistryError);
    }
    copy()
        .filter(|value| value.len() <= limit as usize && value.is_ascii())
        .ok_or(RegistryError)
}

fn copy_chunk(
    length: u32,
    remaining: usize,
    copy: impl FnOnce() -> Vec<u8>,
) -> Result<Zeroizing<Vec<u8>>, RegistryError> {
    if length == 0 || length as usize > remaining {
        return Err(RegistryError);
    }
    let bytes = Zeroizing::new(copy());
    if bytes.len() != length as usize {
        return Err(RegistryError);
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests;
