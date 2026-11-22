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
pub use request::InputError;

#[cfg(target_arch = "wasm32")]
const CONSTRUCTION_BYTES: usize = 8 * 1024 * 1024;

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
    use crate::worker_entry::begin_production_snapshot;

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
    #[test]
    fn construction_refuses_busy_and_credentials_use_the_carried_budget() {
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
            assert!(begin_production_snapshot("f=json").is_err());
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
            let production = begin_production_snapshot("f=json").unwrap();
            assert!(begin_production_snapshot("f=json").is_err());
            assert!(matches!(
                StagingConstruction::begin(empty_input(0)),
                Err(StagingRejection::BusyOrCapacity)
            ));
            drop(production);
            assert!(StagingConstruction::begin(empty_input(0)).is_ok());
        }
    }
}
