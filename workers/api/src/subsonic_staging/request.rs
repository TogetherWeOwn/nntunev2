//! Bounded collection from the raw Worker request. This does not take a permit.

use worker::web_sys;

use super::{ConstructionBudget, StagingInput, CONSTRUCTION_BYTES};
use crate::subsonic_http::{StagingBody, StagingBodyError};
pub use crate::subsonic_request_metadata::InputError;
use crate::subsonic_request_metadata::{
    admit_metadata, BodyPlan, RequestMetadata, CAPACITY, TOO_LARGE,
};

impl From<StagingBodyError> for InputError {
    fn from(error: StagingBodyError) -> Self {
        match error {
            StagingBodyError::TooLarge => TOO_LARGE,
            StagingBodyError::Unreadable => Self::Unreadable,
            StagingBodyError::Capacity => CAPACITY,
            StagingBodyError::Parameters(error) => error.into(),
        }
    }
}

impl StagingInput {
    /// After the staging arming gate, but before wrapping the platform request
    /// in the Worker SDK or allocating request strings in Rust.
    pub async fn collect(request: &web_sys::Request) -> Result<Self, InputError> {
        Self::collect_with_budget(request, ConstructionBudget::new(CONSTRUCTION_BYTES)).await
    }

    async fn collect_with_budget(
        request: &web_sys::Request,
        mut budget: ConstructionBudget,
    ) -> Result<Self, InputError> {
        let RequestMetadata {
            method,
            path,
            query,
            body,
        } = admit_metadata(request, &mut budget)?;
        let (form, budget) = match body {
            BodyPlan::Skip => (String::new(), budget),
            BodyPlan::Form { declared } => {
                StagingBody::new(declared, budget)?
                    .collect_worker(request, Some(&query))
                    .await?
            }
        };
        Ok(Self {
            method,
            path,
            query,
            form,
            budget,
        })
    }
}

#[cfg(feature = "staging-body-wasm-test")]
mod wasm_test {
    use super::*;
    use crate::subsonic_http::STAGING_SOURCE_BYTES;
    use crate::subsonic_staging::StagingConstruction;
    use worker::wasm_bindgen::{self, prelude::*};

    #[wasm_bindgen]
    pub async fn staging_input_test(
        request: web_sys::Request,
        limit: Option<u32>,
    ) -> Result<u32, JsValue> {
        let input = if let Some(limit) = limit {
            StagingInput::collect_with_budget(&request, ConstructionBudget::new(limit as usize))
                .await
        } else {
            StagingInput::collect(&request).await
        }
        .map_err(|error| JsValue::from_str(&format!("collect:{error:?}")))?;
        assert_eq!(input.path.capacity(), input.path.len());
        assert_eq!(input.query.capacity(), input.query.len());
        assert_eq!(
            input.form.capacity(),
            if input.form.is_empty() {
                0
            } else {
                STAGING_SOURCE_BYTES
            }
        );
        let mut expected = ConstructionBudget::new(usize::MAX);
        for cap in [
            input.path.capacity(),
            input.query.capacity(),
            input.form.capacity(),
        ] {
            if cap != 0 {
                expected.charge(cap, 1, 1).unwrap();
            }
        }
        let mut scope = StagingConstruction::begin(input)
            .map_err(|reason| JsValue::from_str(&format!("begin:{reason:?}")))?;
        assert_eq!(scope.input.budget.charged(), expected.charged());
        assert!(super::super::CONSTRUCTION_SLOT.try_acquire().is_none());
        scope
            .parse_credentials()
            .map_err(|error| JsValue::from_str(&format!("credentials:{error:?}")))?;
        Ok(scope.input.budget.charged() as u32)
    }
}
