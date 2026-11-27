//! Bounded collection from the raw Worker request. This does not take a permit.

use worker::js_sys::{Function, JsString};
use worker::wasm_bindgen::{JsCast, JsValue};
use worker::web_sys;

use super::{admits_form_media_type, ConstructionBudget, StagingInput, CONSTRUCTION_BYTES};
use crate::subsonic_http::{StagingBody, StagingBodyError};
pub use crate::subsonic_request_metadata::InputError;
use crate::subsonic_request_metadata::{
    admit_metadata, property, BodyPlan, RequestMetadata, CAPACITY, TOO_LARGE,
};

pub(crate) fn request_header(request: &web_sys::Request, name: &str) -> Option<JsString> {
    let headers = request.headers();
    let get: Function = property(headers.as_ref(), "get").ok()?;
    get.call1(headers.as_ref(), &JsValue::from_str(name))
        .ok()?
        .dyn_into()
        .ok()
}

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
                // Metadata identifies candidates by substring. Check the full
                // media type before interpreting any bytes as credentials.
                let value = request_header(request, "content-type").ok_or(InputError::Unreadable)?;
                let units = (0..value.length()).map(|i| value.char_code_at(i) as u16);
                if !admits_form_media_type(units) {
                    return Err(InputError::Unreadable);
                }
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

    async fn check_form_refusals() -> Result<(), JsValue> {
        for content_type in [
            "text/plain; hint=application/x-www-form-urlencoded",
            "application/x-www-form-urlencoded, text/plain",
        ] {
            let headers = web_sys::Headers::new()?;
            headers.set("content-type", content_type)?;
            let init = web_sys::RequestInit::new();
            init.set_method("POST");
            init.set_headers(headers.as_ref());
            init.set_body(&JsValue::from_str("u=fixture"));
            let request = web_sys::Request::new_with_str_and_init(
                "https://fixture.example/rest/ping",
                &init,
            )?;
            assert_eq!(
                StagingInput::collect(&request).await.err(),
                Some(InputError::Unreadable)
            );
            assert!(!request.body_used());
        }
        Ok(())
    }

    #[wasm_bindgen]
    pub async fn staging_input_test(
        request: web_sys::Request,
        limit: Option<u32>,
    ) -> Result<u32, JsValue> {
        // The Node suite reaches the raw Worker collector through this adapter.
        check_form_refusals().await?;
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
