use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::{Config, Env};
use crate::{
    private_catalogue_read::PublicationScope,
    private_playback::{browser::BrowserScope, PrivatePlayback},
};

pub(super) fn enabled(env: &Env) -> bool {
    env.var("PRIVATE_PLAYBACK_ENABLED")
        .ok()
        .is_some_and(|value| value.to_string() == "true")
}

pub(crate) fn hex<const N: usize>(raw: &str) -> Option<[u8; N]> {
    if raw.len() != N.checked_mul(2)? || !raw.as_bytes().iter().all(u8::is_ascii_hexdigit) {
        return None;
    }
    let mut bytes = [0; N];
    for (i, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&raw[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(bytes)
}

pub(crate) fn uuid_v4(raw: &str) -> Option<[u8; 16]> {
    if !crate::meter_sink::is_uuid_shaped(raw) {
        return None;
    }
    let id = hex::<16>(&raw.replace('-', ""))?;
    is_uuid_v4(&id).then_some(id)
}

pub(crate) fn is_uuid_v4(id: &[u8; 16]) -> bool {
    id[6] >> 4 == 4 && id[8] >> 6 == 2
}

pub(super) struct Settings {
    environment: [u8; 16],
    library: [u8; 16],
    provider: [u8; 16],
    bucket: String,
    pub(super) root_version: [u8; 16],
}

impl Settings {
    pub(super) fn load(env: &Env) -> Option<Self> {
        if !enabled(env) {
            return None;
        }
        let var = |key| env.var(key).ok().map(|value| value.to_string());
        let id = |key| uuid_v4(&var(key)?);
        let bucket = var("PRIVATE_PLAYBACK_S3_BUCKET")?;
        if !nntune_core::s3_locator::valid_bucket(&bucket) {
            return None;
        }
        Some(Self {
            environment: id("PRIVATE_PLAYBACK_ENVIRONMENT")?,
            library: id("PRIVATE_PLAYBACK_LIBRARY")?,
            provider: id("PRIVATE_PLAYBACK_S3_CONFIGURATION")?,
            bucket,
            root_version: id("PRIVATE_PLAYBACK_ROOT_VERSION")?,
        })
    }

    pub(super) fn scope(&self) -> BrowserScope<'_> {
        BrowserScope {
            publication: PublicationScope {
                environment: self.environment,
                library: self.library,
                provider_configuration: self.provider,
                bucket: &self.bucket,
            },
            root_version: self.root_version,
        }
    }
}

pub(super) fn service(cfg: &Config) -> Option<PrivatePlayback<'_>> {
    Some(PrivatePlayback {
        policy: Some(cfg.private_listening.as_ref()?),
        grant_secret: cfg.play_grant_secret.as_deref()?,
    })
}

pub(super) fn now() -> SystemTime {
    UNIX_EPOCH + Duration::from_millis(super::now_millis())
}
