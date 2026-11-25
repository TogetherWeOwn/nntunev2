//! Run the real API dispatcher with an isolated environment and no DB bindings.

use wasm_bindgen::{prelude::*, JsCast};
use worker::web_sys;

#[wasm_bindgen]
pub async fn private_trial_dispatch_test(
    request: web_sys::Request,
    env: JsValue,
) -> Result<web_sys::Response, JsValue> {
    crate::worker_entry::dispatch(request.into(), env.unchecked_into())
        .await
        .map(Into::into)
        .map_err(|_| JsValue::from_str("private trial dispatch failed"))
}
