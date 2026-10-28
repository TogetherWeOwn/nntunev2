//! Shared limiter adapter and deliberately closed Subsonic release boundary.
//!
//! No numerical limit defaults, credentials, or deployment switches live here.
//! Runtime enablement requires a separately reviewed source change after the
//! schema, provisioning, legacy-continuity and implementation gates are proved.

use crate::handlers::StoreError;
use crate::subsonic_auth::{SubsonicAuthAttempt, SubsonicAuthFault, SubsonicAuthLimiter};
use serde::Deserialize;
use std::future::Future;

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Budget {
    max: u64,
    window_secs: u64,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubsonicLimits {
    general: Budget,
    auth_attempt_ip: Budget,
    credential: Budget,
}

impl SubsonicLimits {
    pub fn parse(raw: &str) -> Result<Self, StoreError> {
        let limits: Self = serde_json::from_str(raw).map_err(|_| StoreError)?;
        for budget in [limits.general, limits.auth_attempt_ip, limits.credential] {
            if budget.max == 0
                || budget.max > u64::from(crate::rate_limit::MAX_BUDGET_SLOTS)
                || budget.window_secs == 0
                || i64::try_from(budget.window_secs).is_err()
            {
                return Err(StoreError);
            }
        }
        Ok(limits)
    }
}

/// Port to the existing durable auth-rate-limit slot authority. This is never
/// implemented by an isolate-local map in a deployed request path.
pub trait SubsonicLimitStore {
    fn reserve_subsonic_limit(
        &self,
        scope_key: &str,
        action: &str,
        limit: u64,
        window_secs: u64,
        now: u64,
    ) -> impl Future<Output = Result<bool, StoreError>>;
}

pub struct SharedSubsonicLimiter<S> {
    store: S,
    limits: SubsonicLimits,
    now: u64,
}

impl<S> SharedSubsonicLimiter<S> {
    pub fn new(store: S, limits: SubsonicLimits, now: u64) -> Self {
        Self { store, limits, now }
    }
}

impl<S: SubsonicLimitStore> SharedSubsonicLimiter<S> {
    async fn claim(&self, key: &str, action: &str, budget: Budget) -> Result<bool, StoreError> {
        self.store
            .reserve_subsonic_limit(key, action, budget.max, budget.window_secs, self.now)
            .await
    }
}

impl<S: SubsonicLimitStore> SubsonicAuthLimiter for SharedSubsonicLimiter<S> {
    async fn check_general(&self, _attempt: &SubsonicAuthAttempt) -> Result<bool, StoreError> {
        self.claim("subsonic", "subsonic.general", self.limits.general)
            .await
    }
    async fn check_auth_attempt(&self, attempt: &SubsonicAuthAttempt) -> Result<bool, StoreError> {
        if attempt.ip.parse::<std::net::IpAddr>().is_err() {
            return Ok(false);
        }
        let key = crate::handlers::rate_limit_ip_key(&attempt.ip);
        self.claim(
            &key,
            "subsonic.auth_attempt_ip",
            self.limits.auth_attempt_ip,
        )
        .await
    }
    async fn check_credential(&self, id: &str) -> Result<bool, StoreError> {
        if id.is_empty() {
            return Ok(false);
        }
        self.claim(id, "subsonic.credential", self.limits.credential)
            .await
    }
}

pub struct SubsonicRuntimeSettings {
    keyring: String,
    limits: SubsonicLimits,
}

impl SubsonicRuntimeSettings {
    pub fn into_parts(self) -> (String, SubsonicLimits) {
        (self.keyring, self.limits)
    }
}

// Deliberate release fence, NOT an environment flag. No binding or variable can
// authorize success while provisioning/legacy continuity and runtime gates are
// unproved. Removing this fence requires the exact-head security/release gates.
const SUBSONIC_RELEASE_READY: bool = false;

/// The two readers are separated so the pepper can only come from a secret,
/// while the non-secret budget configuration comes from ordinary variables.
pub fn load_subsonic_runtime_settings(
    secret: impl FnOnce() -> Option<String>,
    limits: impl FnOnce() -> Option<String>,
) -> Result<SubsonicRuntimeSettings, SubsonicAuthFault> {
    if !SUBSONIC_RELEASE_READY {
        return Err(SubsonicAuthFault::Configuration);
    }
    let keyring = secret()
        .filter(|raw| !raw.is_empty())
        .ok_or(SubsonicAuthFault::Keyring)?;
    let limits = limits().ok_or(SubsonicAuthFault::Configuration)?;
    let limits = SubsonicLimits::parse(&limits).map_err(|_| SubsonicAuthFault::Configuration)?;
    Ok(SubsonicRuntimeSettings { keyring, limits })
}

#[cfg(test)]
mod builder_tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::task::{Context, Poll, Waker};

    const LIMITS: &str = r#"{"general":{"max":7,"window_secs":300},"auth_attempt_ip":{"max":3,"window_secs":60},"credential":{"max":2,"window_secs":120}}"#;
    type Claims = Rc<RefCell<Vec<(String, String, u64, u64, u64)>>>;
    struct Slots {
        claims: Claims,
        result: Result<bool, StoreError>,
    }
    impl SubsonicLimitStore for Slots {
        async fn reserve_subsonic_limit(
            &self,
            key: &str,
            action: &str,
            limit: u64,
            window: u64,
            now: u64,
        ) -> Result<bool, StoreError> {
            self.claims
                .borrow_mut()
                .push((key.into(), action.into(), limit, window, now));
            match &self.result {
                Ok(value) => Ok(*value),
                Err(_) => Err(StoreError),
            }
        }
    }
    fn ready<F: Future>(future: F) -> F::Output {
        let mut future = std::pin::pin!(future);
        match future
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
        {
            Poll::Ready(value) => value,
            Poll::Pending => panic!("in-process slot double must be ready"),
        }
    }
    fn limiter(result: Result<bool, StoreError>) -> (SharedSubsonicLimiter<Slots>, Claims) {
        let claims = Rc::new(RefCell::new(Vec::new()));
        let limiter = SharedSubsonicLimiter::new(
            Slots {
                claims: claims.clone(),
                result,
            },
            SubsonicLimits::parse(LIMITS).unwrap(),
            1_800_000_000,
        );
        (limiter, claims)
    }

    #[test]
    fn release_hold_reads_no_secrets_or_limit_configuration() {
        let result = load_subsonic_runtime_settings(
            || panic!("held runtime read a secret"),
            || panic!("held runtime read limit configuration"),
        );
        assert!(matches!(result, Err(SubsonicAuthFault::Configuration)));
    }

    #[test]
    fn explicit_limits_are_required_and_never_default_or_clamp() {
        assert!(SubsonicLimits::parse(LIMITS).is_ok());
        for raw in [
            "",
            "null",
            "{}",
            r#"{"general":{"max":7,"window_secs":300}}"#,
        ] {
            assert!(SubsonicLimits::parse(raw).is_err());
        }
        for invalid in ["0", "101", "-1", "1.5", "\"7\""] {
            let raw = LIMITS.replacen("\"max\":7", &format!("\"max\":{invalid}"), 1);
            assert!(SubsonicLimits::parse(&raw).is_err());
        }
        assert!(
            SubsonicLimits::parse(&LIMITS.replace("\"window_secs\":300", "\"window_secs\":0"))
                .is_err()
        );
        assert!(
            SubsonicLimits::parse(&LIMITS.replace("\"max\":7", "\"max\":7,\"max\":8")).is_err()
        );
    }

    #[test]
    fn every_gate_claims_the_shared_store_with_the_explicit_budget() {
        let (limiter, claims) = limiter(Ok(true));
        let attempt = SubsonicAuthAttempt {
            ip: "192.0.2.10".into(),
        };
        assert!(ready(limiter.check_general(&attempt)).unwrap());
        assert!(ready(limiter.check_auth_attempt(&attempt)).unwrap());
        assert!(ready(limiter.check_credential("credential-id")).unwrap());
        assert_eq!(
            *claims.borrow(),
            [
                (
                    "subsonic".into(),
                    "subsonic.general".into(),
                    7,
                    300,
                    1_800_000_000
                ),
                (
                    "192.0.2.10".into(),
                    "subsonic.auth_attempt_ip".into(),
                    3,
                    60,
                    1_800_000_000
                ),
                (
                    "credential-id".into(),
                    "subsonic.credential".into(),
                    2,
                    120,
                    1_800_000_000
                ),
            ]
        );
    }

    #[test]
    fn denied_and_failed_shared_reservations_never_turn_into_allow() {
        for verdict in [Ok(false), Err(StoreError)] {
            let (limiter, _) = limiter(verdict);
            let attempt = SubsonicAuthAttempt {
                ip: "192.0.2.10".into(),
            };
            for result in [
                ready(limiter.check_general(&attempt)),
                ready(limiter.check_auth_attempt(&attempt)),
                ready(limiter.check_credential("credential-id")),
            ] {
                assert!(!matches!(result, Ok(true)));
            }
        }
    }

    #[test]
    fn ip_gate_groups_ipv6_slash64_and_refuses_missing_or_injected_sources() {
        let (limiter, claims) = limiter(Ok(true));
        for ip in ["2001:db8:1:2::1", "2001:db8:1:2::ffff"] {
            assert!(
                ready(limiter.check_auth_attempt(&SubsonicAuthAttempt { ip: ip.into() })).unwrap()
            );
        }
        assert_eq!(claims.borrow()[0].0, "2001:db8:1:2::");
        assert_eq!(claims.borrow()[1].0, "2001:db8:1:2::");
        for ip in ["", "unknown", "192.0.2.10\nforged", "not-an-ip"] {
            assert!(
                !ready(limiter.check_auth_attempt(&SubsonicAuthAttempt { ip: ip.into() })).unwrap()
            );
        }
        assert_eq!(claims.borrow().len(), 2);
    }
}
