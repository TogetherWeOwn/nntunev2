use std::cell::OnceCell;

use ::worker::{wasm_bindgen::JsValue, web_sys, Fetcher, ResponseBody};
use futures_util::StreamExt;
use nntune_core::{
    digest_enrollment::{PlaybackManifest, StoredManifest},
    media_envelope::EnvelopeContext,
    playback_custody::{self, CustodySnapshot, LEASE_PATH, RESOLVE_PATH},
    s3_locator::{S3Binding, S3Locator, S3MediaMetadata},
};
use zeroize::Zeroizing;

use super::*;
use crate::{
    enrolled_playback::s3_transport::{hex, now, Deadline, S3Body, S3Reader},
    runtime::{API_SERVICE_BINDING, POOL_BINDING},
};

pub(super) struct WorkerSource<'a> {
    api: Fetcher,
    bearer: &'a str,
    custody_secret: Zeroizing<String>,
    reader: S3Reader,
    root_version: [u8; 16],
    root: Zeroizing<[u8; 32]>,
    resolved: OnceCell<Resolved>,
}

struct Resolved {
    snapshot: CustodySnapshot,
    context: EnvelopeContext,
    media: S3MediaMetadata,
    manifest: StoredManifest,
    record: [u8; 230],
}

impl<'a> WorkerSource<'a> {
    pub(super) fn from_env(
        env: &Env,
        bearer: &'a str,
        observation: Option<Rc<ReadObservation>>,
    ) -> Option<Self> {
        let var = |name| env.var(name).ok().map(|value| value.to_string());
        let secret = |name| {
            env.secret(name)
                .ok()
                .map(|value| Zeroizing::new(value.to_string()))
        };
        if var("PRIVATE_PLAYBACK_ENABLED")?.as_str() != "true" {
            return None;
        }
        let api = env.service(API_SERVICE_BINDING).ok()?;
        env.durable_object(POOL_BINDING).ok()?;
        let custody_secret = secret(playback_custody::SECRET_BINDING)?;
        if custody_secret.len() < 32 {
            return None;
        }
        let mut reader = S3Reader::new(
            uuid(&var("PRIVATE_PLAYBACK_S3_CONFIGURATION")?)?,
            var("PRIVATE_PLAYBACK_S3_BUCKET")?,
            &var("PRIVATE_PLAYBACK_S3_ENDPOINT")?,
            var("PRIVATE_PLAYBACK_S3_REGION")?,
            secret("PRIVATE_PLAYBACK_S3_ACCESS_KEY_ID")?.to_string(),
            secret("PRIVATE_PLAYBACK_S3_SECRET_ACCESS_KEY")?,
        )
        .ok()?;
        reader.observation = observation;
        Some(Self {
            api,
            bearer,
            custody_secret,
            reader,
            root_version: uuid(&var("PRIVATE_PLAYBACK_ROOT_VERSION")?)?,
            root: Zeroizing::new(decode_hex(&secret("PRIVATE_PLAYBACK_ROOT_KEY")?)?),
            resolved: OnceCell::new(),
        })
    }

    async fn request(&self, path: &str, grant: &Grant) -> Option<(Response, Deadline)> {
        let exp = UNIX_EPOCH.checked_add(Duration::from_secs(grant.exp_unix))?;
        let abort = Deadline::new(exp.min(now() + Duration::from_secs(5))).ok()?;
        let signature =
            playback_custody::signature(self.custody_secret.as_bytes(), path, self.bearer)?;
        let headers = web_sys::Headers::new().ok()?;
        headers
            .set(
                "authorization",
                &format!("{} {}", playback_custody::AUTH_SCHEME, hex(&signature)),
            )
            .ok()?;
        let init = web_sys::RequestInit::new();
        init.set_method("POST");
        init.set_headers(headers.as_ref());
        init.set_redirect(web_sys::RequestRedirect::Error);
        init.set_signal(Some(&abort.signal()));
        init.set_body(&JsValue::from_str(self.bearer));
        let req =
            web_sys::Request::new_with_str_and_init(&format!("https://nntune-api{path}"), &init)
                .ok()?;
        abort.check().ok()?;
        let response = self.api.fetch_request(Request::from(req)).await.ok()?;
        abort.check().ok()?;
        Some((response, abort))
    }

    async fn load(&self, grant: &Grant) -> Option<Resolved> {
        let (mut response, abort) = self.request(RESOLVE_PATH, grant).await?;
        if response.status_code() != 200 {
            return None;
        }
        let mut stream = Box::pin(response.stream().ok()?);
        let mut bytes = Vec::new();
        while let Some(chunk) = stream.next().await {
            abort.check().ok()?;
            let chunk = chunk.ok()?;
            if bytes.len().saturating_add(chunk.len()) > playback_custody::MAX_RESPONSE_BYTES {
                return None;
            }
            bytes.extend_from_slice(&chunk);
        }
        abort.check().ok()?;
        let mut snapshot: CustodySnapshot = serde_json::from_slice(&bytes).ok()?;
        let hashes = std::mem::take(&mut snapshot.page_hashes);
        let pins = snapshot.pins()?;
        if snapshot.bearer_sha256 != grant::sha256(self.bearer.as_bytes())
            || pins.root_version_id != self.root_version
            || now() >= pins.usable_until
            || !self
                .reader
                .matches(pins.provider_configuration, &snapshot.bucket)
        {
            return None;
        }
        let context = EnvelopeContext {
            identities: snapshot.identities,
            plaintext_len: pins.plaintext_len,
            header: snapshot.header.as_slice().try_into().ok()?,
        };
        let record = snapshot.envelope_record.as_slice().try_into().ok()?;
        let manifest = StoredManifest::from_committed_storage(&pins, context.header, hashes)?;
        if !manifest.authenticate(&self.root, &context, &record) {
            return None;
        }
        let locator = S3Locator::new(
            snapshot.bucket.clone(),
            snapshot.key.clone(),
            S3Binding::VersionId(snapshot.media_version.clone()),
        )
        .ok()?;
        let media = S3MediaMetadata::from_trusted_context(locator, &context)?;
        abort.check().ok()?;
        if now() >= pins.usable_until {
            return None;
        }
        Some(Resolved {
            snapshot,
            context,
            media,
            manifest,
            record,
        })
    }
}

impl PrivateSource for WorkerSource<'_> {
    type Body = S3Body;

    async fn resolve(&self, grant: &Grant) -> Option<CatalogueMedia<'_>> {
        self.resolved.set(self.load(grant).await?).ok()?;
        let resolved = self.resolved.get()?;
        Some(CatalogueMedia {
            reference: resolved.snapshot.catalogue_reference,
            pins: resolved.snapshot.pins()?,
            media: EnrolledMedia {
                media: &resolved.media,
                manifest: PlaybackManifest::Stored(&resolved.manifest),
                context: &resolved.context,
                root: &self.root,
                expected_record: &resolved.record,
                retained_record: &resolved.record,
            },
        })
    }

    async fn lease_live(&self, grant: &Grant) -> bool {
        self.request(LEASE_PATH, grant)
            .await
            .is_some_and(|(response, _)| {
                response.status_code() == 204 && matches!(response.body(), ResponseBody::Empty)
            })
    }

    async fn media(&self, get: S3Get<'_>) -> io::Result<S3GetReply<Self::Body>> {
        self.reader.open(get).await
    }

    async fn page(
        &self,
        bucket: &str,
        pin: ReadbackPin<'_>,
        length: u32,
        until: SystemTime,
    ) -> io::Result<(String, Vec<u8>)> {
        self.reader.page(bucket, pin, length, until).await
    }
}

fn decode_hex<const N: usize>(value: &str) -> Option<[u8; N]> {
    if value.len() != 2 * N
        || !value
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    {
        return None;
    }
    let mut out = [0; N];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&value[2 * i..2 * i + 2], 16).ok()?;
    }
    Some(out)
}

fn uuid(value: &str) -> Option<[u8; 16]> {
    if value.len() != 36 || [8, 13, 18, 23].iter().any(|&i| value.as_bytes()[i] != b'-') {
        return None;
    }
    let id: [u8; 16] = decode_hex(&value.replace('-', ""))?;
    (id[6] >> 4 == 4 && id[8] >> 6 == 2).then_some(id)
}
