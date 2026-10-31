//! Innertube client profiles and `/player` request construction.
//!
//! Day-1 extraction uses the `VISIONOS` client: per host research
//! (2026-10-21, live tests) it returns plain non-SABR stream URLs with no
//! PO token and no `n` parameter. Profile values are pinned to the
//! `youtubei.js` `Constants.CLIENTS.VISIONOS` table (verified 2026-10-21
//! against upstream `main`).
//!
//! Profiles carry no key material: the Innertube API key (`?key=`) is
//! runtime config supplied by the Worker (KV config, extracted from
//! YouTube's public homepage JS at deploy time), passed to [`player_url`]
//! by the caller. Key-shaped literals must never be committed here; the
//! gitleaks gate fails the build on them.
//!
//! Client-table maintenance is expected monthly (see the spike spec in
//! `docs/youtube-backend-spike.md`): [`CLIENT_ORDER`] is the compiled-in
//! default; reading an ordered client list from KV config arrives with the
//! extraction wiring.

/// An Innertube client identity for `/player` requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClientProfile {
    /// `context.client.clientName`, e.g. `"VISIONOS"`.
    pub name: &'static str,
    /// `context.client.clientVersion`, e.g. `"1.02"`.
    pub version: &'static str,
    /// `User-Agent` header for player requests.
    pub user_agent: &'static str,
    /// `X-YouTube-Client-Name` header value (`"101"` for VISIONOS).
    pub client_name_id: &'static str,
    /// `context.client.platform` (`"MOBILE"` for VISIONOS).
    pub platform: &'static str,
    /// `context.client.deviceMake`.
    pub device_make: Option<&'static str>,
    /// `context.client.deviceModel`.
    pub device_model: Option<&'static str>,
    /// `context.client.osName`.
    pub os_name: Option<&'static str>,
    /// `context.client.osVersion`.
    pub os_version: Option<&'static str>,
}

/// The VISIONOS profile (`youtubei.js` `Constants.CLIENTS.VISIONOS`).
pub const VISIONOS: ClientProfile = ClientProfile {
    name: "VISIONOS",
    version: "1.02",
    user_agent: "Mozilla/5.0 (Macintosh; Intel Mac OS X 15_7_3) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/26.0 Safari/605.1.15",
    client_name_id: "101",
    platform: "MOBILE",
    device_make: Some("Apple"),
    device_model: Some("RealityDevice17,1"),
    os_name: Some("visionOS"),
    os_version: Some("26.5.23O471"),
};

/// Compiled-in client try-order; the Worker overrides it from KV config.
pub const CLIENT_ORDER: &[ClientProfile] = &[VISIONOS];

/// Default Innertube host for `/player` requests.
pub const INNERTUBE_HOST: &str = "www.youtube.com";

/// `/player` endpoint URL for a bare `host` (no scheme). The caller
/// supplies `api_key` from runtime config (never a committed literal).
pub fn player_url(host: &str, api_key: &str) -> String {
    format!("https://{host}/youtubei/v1/player?key={api_key}&prettyPrint=false")
}

/// `/player` POST body for `profile` and `video_id`.
///
/// Optional device fields the profile does not set are omitted (never
/// sent as explicit nulls).
pub fn player_body(profile: &ClientProfile, video_id: &str) -> serde_json::Value {
    let mut client = serde_json::Map::from_iter([
        ("clientName".to_string(), profile.name.into()),
        ("clientVersion".to_string(), profile.version.into()),
        ("platform".to_string(), profile.platform.into()),
        ("clientScreen".to_string(), "WATCH".into()),
        ("hl".to_string(), "en".into()),
        ("gl".to_string(), "US".into()),
    ]);
    for (key, value) in [
        ("deviceMake", profile.device_make),
        ("deviceModel", profile.device_model),
        ("osName", profile.os_name),
        ("osVersion", profile.os_version),
    ] {
        if let Some(value) = value {
            client.insert(key.to_string(), value.into());
        }
    }
    serde_json::json!({
        "context": {"client": client},
        "videoId": video_id,
        "contentCheckOk": true,
        "racyCheckOk": true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn visionos_profile_matches_upstream_table() {
        assert_eq!(VISIONOS.name, "VISIONOS");
        assert_eq!(VISIONOS.version, "1.02");
        assert_eq!(VISIONOS.device_make, Some("Apple"));
        assert_eq!(VISIONOS.device_model, Some("RealityDevice17,1"));
        assert_eq!(VISIONOS.os_name, Some("visionOS"));
        assert_eq!(VISIONOS.os_version, Some("26.5.23O471"));
        assert_eq!(VISIONOS.client_name_id, "101");
        assert_eq!(VISIONOS.platform, "MOBILE");
    }

    #[test]
    fn player_url_shape() {
        assert_eq!(
            player_url("www.youtube.com", "KEY"),
            "https://www.youtube.com/youtubei/v1/player?key=KEY&prettyPrint=false"
        );
    }

    #[test]
    fn player_body_carries_client_and_video() {
        let body = player_body(&VISIONOS, "dQw4w9WgXcQ");
        assert_eq!(body["context"]["client"]["clientName"], "VISIONOS");
        assert_eq!(body["context"]["client"]["clientVersion"], "1.02");
        assert_eq!(
            body["context"]["client"]["deviceModel"],
            "RealityDevice17,1"
        );
        assert_eq!(body["videoId"], "dQw4w9WgXcQ");
        assert_eq!(body["contentCheckOk"], true);
        assert_eq!(body["racyCheckOk"], true);
    }

    #[test]
    fn client_order_starts_with_visionos() {
        assert_eq!(CLIENT_ORDER.first(), Some(&VISIONOS));
    }
}
