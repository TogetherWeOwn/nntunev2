//! Unmounted lease checks for the staging catalogue experiment.

const MAX_LEASE_SECONDS: u64 = 20 * 60;

/// These facts come from deployment bindings, not from the lease or headers.
pub struct Deployment<'a> {
    pub is_staging: bool,
    pub staging_host: &'a str,
    pub revision: &'a str,
    pub now_utc_seconds: u64,
}

pub struct Lease<'a> {
    synthetic_principal: &'a str,
}

impl<'a> Lease<'a> {
    /// `raw` is `v1:<sha>:<issued UTC epoch>:<expires UTC epoch>:<user id>`.
    /// Check before reading credentials, opening a database, or building rows.
    pub fn admit(
        deployment: &Deployment<'_>,
        https_host: Option<&str>,
        enable: Option<&str>,
        raw: Option<&'a str>,
    ) -> Option<Self> {
        if !deployment.is_staging
            || https_host != Some(deployment.staging_host)
            || !valid_host(deployment.staging_host)
            || enable != Some("1")
        {
            return None;
        }
        let mut fields = raw.filter(|raw| raw.len() <= 224)?.split(':');
        if fields.next()? != "v1" {
            return None;
        }
        let revision = fields.next()?;
        if revision != deployment.revision
            || revision.len() != 40
            || !revision
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return None;
        }
        let issued = epoch(fields.next()?)?;
        let expires = epoch(fields.next()?)?;
        let synthetic_principal = fields.next()?;
        if fields.next().is_some()
            || synthetic_principal.is_empty()
            || synthetic_principal.len() > 128
            || !synthetic_principal
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            || expires.checked_sub(issued)? > MAX_LEASE_SECONDS
            || !(issued..expires).contains(&deployment.now_utc_seconds)
        {
            return None;
        }
        Some(Self {
            synthetic_principal,
        })
    }

    /// The supplied ID must come from an already authenticated principal.
    pub fn allows_principal(&self, authenticated_user_id: &str) -> bool {
        self.synthetic_principal == authenticated_user_id
    }
}

fn valid_host(host: &str) -> bool {
    // The binding is an RP ID, so accept a bare DNS name in the same
    // lowercase form as the request URL's host.
    host.len() <= 253
        && host.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        })
        && matches!(url::Host::parse(host), Ok(url::Host::Domain(name)) if name == host)
}

fn epoch(value: &str) -> Option<u64> {
    if value.bytes().all(|b| b.is_ascii_digit()) {
        value.parse().ok()
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOST: &str = "stage.example.test";
    const SHA: &str = "0123456789abcdef0123456789abcdef01234567";

    #[test]
    fn lease_is_default_off_and_bound_to_the_exact_staging_deployment() {
        let mut deployment = Deployment {
            is_staging: true,
            staging_host: HOST,
            revision: SHA,
            now_utc_seconds: 1_000,
        };
        let raw = format!("v1:{SHA}:1000:2200:fixture-user");
        let lease = Lease::admit(&deployment, Some(HOST), Some("1"), Some(&raw)).unwrap();
        assert!(lease.allows_principal("fixture-user"));
        assert!(!lease.allows_principal("ordinary-user"));
        for (host, enable, config) in [
            (None, Some("1"), Some(raw.as_str())),
            (Some("other.example.test"), Some("1"), Some(raw.as_str())),
            (Some(HOST), None, Some(raw.as_str())),
            (Some(HOST), Some("0"), Some(raw.as_str())),
            (Some(HOST), Some("1"), None),
        ] {
            assert!(Lease::admit(&deployment, host, enable, config).is_none());
        }
        deployment.is_staging = false;
        assert!(Lease::admit(&deployment, Some(HOST), Some("1"), Some(&raw)).is_none());
        deployment.is_staging = true;
        deployment.revision = "ffffffffffffffffffffffffffffffffffffffff";
        assert!(Lease::admit(&deployment, Some(HOST), Some("1"), Some(&raw)).is_none());
        deployment.revision = SHA;
        deployment.now_utc_seconds = 2_200;
        assert!(Lease::admit(&deployment, Some(HOST), Some("1"), Some(&raw)).is_none());
        deployment.now_utc_seconds = 1_000;
        for suffix in [
            "999:999:fixture",
            "1001:2100:fixture",
            "1000:2201:fixture",
            "1000:999:fixture",
            "+1000:2100:fixture",
            "1000:2200:",
            "1000:2200:x:y",
            "1000:2200:é",
        ] {
            let invalid = format!("v1:{SHA}:{suffix}");
            assert!(
                Lease::admit(&deployment, Some(HOST), Some("1"), Some(&invalid)).is_none(),
                "{suffix}"
            );
        }
    }
}
