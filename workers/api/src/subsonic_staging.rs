//! Unmounted staging request ownership and construction admission.

use nntune_subsonic::auth::Credentials;
use nntune_subsonic::construction_budget::ConstructionBudget;
use nntune_subsonic::construction_permit::{ConstructionPermit, CONSTRUCTION_SLOT};
use nntune_subsonic::staging_rejection::StagingRejection;

use crate::subsonic_http::{parse_staging_credentials, CredentialParseError};

#[cfg(target_arch = "wasm32")]
mod dispatch;
#[cfg(any(test, target_arch = "wasm32"))]
mod fixture;
#[cfg(target_arch = "wasm32")]
mod request;

#[cfg(target_arch = "wasm32")]
pub use crate::subsonic_staging_response::respond_fixed_rejection;
#[cfg(target_arch = "wasm32")]
pub use dispatch::{respond_staging, CatalogueSource, StagingApp, StagingDispatch};
#[cfg(target_arch = "wasm32")]
pub use fixture::SyntheticCatalogue;
#[cfg(target_arch = "wasm32")]
pub(crate) use request::request_header;
#[cfg(target_arch = "wasm32")]
pub use request::InputError;

#[cfg(target_arch = "wasm32")]
const CONSTRUCTION_BYTES: usize = 8 * 1024 * 1024;

fn admits_form_media_type(units: impl Iterator<Item = u16>) -> bool {
    fn skip_ows(bytes: &mut std::iter::Peekable<impl Iterator<Item = u8>>) {
        while bytes.next_if(|b| matches!(b, b' ' | b'\t')).is_some() {}
    }

    fn take_token(bytes: &mut std::iter::Peekable<impl Iterator<Item = u8>>) -> bool {
        let is_token = |b: &u8| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(b);
        let nonempty = bytes.next_if(is_token).is_some();
        while bytes.next_if(is_token).is_some() {}
        nonempty
    }

    let form = b"application/x-www-form-urlencoded";
    let mut bytes = units.map(|unit| u8::try_from(unit).unwrap_or(0)).peekable();
    if !bytes.by_ref().take(form.len()).eq(form.iter().copied()) {
        return false;
    }
    loop {
        skip_ows(&mut bytes);
        match bytes.next() {
            None => return true,
            Some(b';') => {}
            _ => return false,
        }
        skip_ows(&mut bytes);
        if matches!(bytes.peek(), None | Some(b';')) {
            continue;
        }
        if !take_token(&mut bytes) {
            return false;
        }
        skip_ows(&mut bytes);
        if bytes.next() != Some(b'=') {
            return false;
        }
        skip_ows(&mut bytes);
        if bytes.next_if_eq(&b'"').is_some() {
            loop {
                match bytes.next() {
                    Some(b'"') => break,
                    Some(b'\\') => match bytes.next() {
                        Some(b'\t' | b' '..=b'~' | 0x80..=0xff) => {}
                        _ => return false,
                    },
                    Some(b'\t' | b' '..=b'~' | 0x80..=0xff) => {}
                    _ => return false,
                }
            }
        } else if !take_token(&mut bytes) {
            return false;
        }
    }
}

/// Validated input and the ledger that paid for its backing storage.
pub struct StagingInput {
    method: &'static str,
    path: String,
    query: String,
    form: String,
    budget: ConstructionBudget,
}

#[must_use = "keep the construction permit through response preparation"]
pub struct StagingConstruction {
    input: StagingInput,
    _permit: ConstructionPermit<'static>,
}

impl StagingConstruction {
    /// Call before decoding credentials, authentication, or database work.
    /// A busy contender neither queues nor starts construction.
    pub fn begin(input: StagingInput) -> Result<Self, StagingRejection> {
        let permit = CONSTRUCTION_SLOT
            .try_acquire()
            .ok_or(StagingRejection::BusyOrCapacity)?;
        Ok(Self {
            input,
            _permit: permit,
        })
    }

    pub fn method(&self) -> &'static str {
        self.input.method
    }

    pub fn path(&self) -> &str {
        &self.input.path
    }

    /// Decode credentials only while holding the construction permit.
    pub fn parse_credentials(&mut self) -> Result<Credentials, CredentialParseError> {
        parse_staging_credentials(
            Some(&self.input.query),
            Some(self.input.form.as_bytes()),
            &mut self.input.budget,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::subsonic_http::{StagingBody, STAGING_SOURCE_BYTES};
    use crate::worker_entry::with_production_snapshot;

    #[test]
    fn form_admission_requires_one_exact_media_type() {
        let form = "application/x-www-form-urlencoded";
        for header in [
            form.to_owned(),
            format!("{form} \t; charset=utf-8"),
            format!(r#"{form}; name="one\",two""#),
        ] {
            assert!(admits_form_media_type(header.encode_utf16()));
        }
        for header in [
            format!("text/plain; hint={form}"),
            format!("{form}-extra"),
            format!("{form}, text/plain"),
            format!("{form}; charset=utf-8, text/plain"),
            format!(r#"{form}; name="one,two"#),
            format!(r#"{form}; name=one",two""#),
            format!(r#"{form}; name="one,two", text/plain"#),
            format!("{form}\u{a0}; charset=utf-8"),
        ] {
            assert!(!admits_form_media_type(header.encode_utf16()), "{header}");
        }
    }

    fn empty_input(limit: usize) -> StagingInput {
        StagingInput {
            method: "POST",
            path: String::new(),
            query: String::new(),
            form: String::new(),
            budget: ConstructionBudget::new(limit),
        }
    }

    // Exercise both cases in one test so they do not race for the real slot.
    #[tokio::test]
    async fn construction_refuses_busy_and_credentials_use_the_carried_budget() {
        let mut expected = ConstructionBudget::new(usize::MAX);
        expected.charge(STAGING_SOURCE_BYTES, 1, 1).unwrap();
        let form_charge = expected.charged();
        expected.charge(1, 1, 1).unwrap();
        expected.charge(6, 1, 1).unwrap();
        let charge = expected.charged();
        for limit in [charge - 1, charge] {
            let mut input = empty_input(limit);
            let mut sink = StagingBody::new(None, input.budget).unwrap();
            sink.push(Ok(b"u=%C3%A9")).unwrap();
            (input.form, input.budget) = sink.finish(None).unwrap();
            let mut scope = StagingConstruction::begin(input).unwrap();
            assert!(matches!(
                StagingConstruction::begin(empty_input(0)),
                Err(StagingRejection::BusyOrCapacity)
            ));
            assert_production_snapshot_is_busy().await;
            assert_eq!(scope.input.budget.charged(), form_charge);
            let result = scope.parse_credentials();
            if limit < charge {
                assert_eq!(result.unwrap_err(), CredentialParseError::Capacity);
            } else {
                let name = result.unwrap().username.unwrap();
                assert_eq!(name, "é");
                assert_eq!(name.capacity(), 6);
                assert_eq!(scope.input.budget.charged(), charge);
            }
            assert!(CONSTRUCTION_SLOT.try_acquire().is_none());
            drop(scope);
        }

        // An in-flight production read blocks admission. Dropping that future
        // releases the slot, even though response preparation never ran.
        {
            let mut production = std::pin::pin!(with_production_snapshot(
                "f=json",
                std::future::pending::<()>(),
            ));
            assert!(futures_util::poll!(production.as_mut()).is_pending());
            assert_production_snapshot_is_busy().await;
            assert_staging_snapshot_is_busy();
        }
        let prepared = with_production_snapshot("f=json", async {
            tokio::task::yield_now().await;
            assert_production_snapshot_is_busy().await;
            assert_staging_snapshot_is_busy();
            "prepared"
        })
        .await
        .unwrap();
        assert_eq!(prepared, "prepared");
        assert!(StagingConstruction::begin(empty_input(0)).is_ok());
    }

    async fn assert_production_snapshot_is_busy() {
        let refusal = with_production_snapshot::<()>("f=json", async {
            panic!("a busy snapshot must skip response preparation");
        })
        .await;
        assert!(refusal.is_err());
    }

    fn assert_staging_snapshot_is_busy() {
        assert!(matches!(
            StagingConstruction::begin(empty_input(0)),
            Err(StagingRejection::BusyOrCapacity)
        ));
    }
}
