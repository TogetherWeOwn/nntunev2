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

fn hex<const N: usize>(raw: &str) -> Option<[u8; N]> {
    if raw.len() != 2 * N || !raw.as_bytes().iter().all(u8::is_ascii_hexdigit) {
        return None;
    }
    let mut bytes = [0; N];
    for (i, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&raw[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(bytes)
}

pub(super) struct Settings {
    environment: [u8; 16],
    library: [u8; 16],
    provider: [u8; 16],
    bucket: String,
    root_version: [u8; 16],
}

impl Settings {
    pub(super) fn load(env: &Env) -> Option<Self> {
        if !enabled(env) {
            return None;
        }
        let var = |key| env.var(key).ok().map(|value| value.to_string());
        let id = |key| {
            let value = var(key)?;
            if !crate::meter_sink::is_uuid_shaped(&value) {
                return None;
            }
            hex(&value.replace('-', ""))
        };
        Some(Self {
            environment: id("PRIVATE_PLAYBACK_ENVIRONMENT")?,
            library: id("PRIVATE_PLAYBACK_LIBRARY")?,
            provider: id("PRIVATE_PLAYBACK_S3_CONFIGURATION")?,
            bucket: var("PRIVATE_PLAYBACK_S3_BUCKET")?,
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
