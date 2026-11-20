//! Inert admission of staging metadata from the raw platform request.
//!
//! The Worker SDK eagerly copies the URL and path in `Request::from`. Keep
//! platform URL parsing and normalized Headers in JS, and charge before making
//! Rust copies of the admitted path and query. This does not read the body.

use nntune_subsonic::construction_budget::ConstructionBudget;
use nntune_subsonic::staging_rejection::StagingRejection;
use worker::js_sys::{self, Array, Function, JsString, Reflect};
use worker::wasm_bindgen::{JsCast, JsValue};
use worker::web_sys;

use crate::subsonic_http::{validate_staging_sources, CredentialParseError, STAGING_SOURCE_BYTES};

const PATH_BYTES: usize = 1024;
const HEADER_BYTES: usize = 8 * 1024;
const FORM: &str = "application/x-www-form-urlencoded";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputError {
    Rejected(StagingRejection),
    UnsupportedMethod,
    Unreadable,
}

pub(crate) const TOO_LARGE: InputError =
    InputError::Rejected(StagingRejection::RequestOrDocumentTooLarge);
pub(crate) const CAPACITY: InputError = InputError::Rejected(StagingRejection::BusyOrCapacity);

impl From<CredentialParseError> for InputError {
    fn from(error: CredentialParseError) -> Self {
        match error {
            CredentialParseError::SourceTooLarge
            | CredentialParseError::TooManyParams
            | CredentialParseError::KeyTooLong
            | CredentialParseError::ValueTooLong => TOO_LARGE,
            CredentialParseError::Capacity => CAPACITY,
            CredentialParseError::MalformedEncoding | CredentialParseError::DuplicateCredential => {
                Self::Unreadable
            }
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum BodyPlan {
    Skip,
    Form { declared: Option<usize> },
}

pub struct RequestMetadata {
    pub method: &'static str,
    pub path: String,
    pub query: String,
    pub body: BodyPlan,
}

/// Call after the staging arming gate, before converting the platform request
/// to a Worker SDK request. The supplied ledger pays for the returned strings.
pub fn admit_metadata(
    request: &web_sys::Request,
    budget: &mut ConstructionBudget,
) -> Result<RequestMetadata, InputError> {
    let raw_method = property::<JsString>(request.as_ref(), "method")?;
    let method = ["GET", "POST"]
        .into_iter()
        .find(|method| is_ascii_literal(&raw_method, method))
        .ok_or(InputError::UnsupportedMethod)?;
    let url = request_url(request)?;
    let path = property::<JsString>(&url, "pathname")?;
    let query = property::<JsString>(&url, "search")?;
    let query_start = u32::from(query.length() != 0); // omit the leading '?'
    if path.length() as usize > PATH_BYTES
        || (query.length() - query_start) as usize > STAGING_SOURCE_BYTES
    {
        return Err(TOO_LARGE);
    }

    let mut header_bytes = 0usize;
    let mut form = false;
    let mut declared = None;
    for entry in request.headers().entries() {
        let pair: Array = entry
            .and_then(JsCast::dyn_into)
            .map_err(|_| InputError::Unreadable)?;
        let name: JsString = pair.get(0).dyn_into().map_err(|_| InputError::Unreadable)?;
        let value: JsString = pair.get(1).dyn_into().map_err(|_| InputError::Unreadable)?;
        // Count normalized Fetch ByteStrings, plus ': ' and CRLF per entry.
        header_bytes = header_bytes
            .checked_add(name.length() as usize)
            .and_then(|n| n.checked_add(value.length() as usize))
            .and_then(|n| n.checked_add(4))
            .filter(|&n| n <= HEADER_BYTES)
            .ok_or(TOO_LARGE)?;
        if is_ascii_literal(&name, "content-type") {
            form = method == "POST" && value.index_of(FORM, 0) >= 0;
        } else if is_ascii_literal(&name, "content-length") {
            declared = Some(value);
        }
    }
    let body = if form {
        BodyPlan::Form {
            declared: declared.as_ref().map(content_length).transpose()?,
        }
    } else {
        BodyPlan::Skip
    };
    let path = copy_url_ascii(budget, &path, 0)?;
    let query = copy_url_ascii(budget, &query, query_start)?;
    validate_staging_sources(Some(&query), None)?;
    Ok(RequestMetadata {
        method,
        path,
        query,
        body,
    })
}

pub(crate) fn request_url(request: &web_sys::Request) -> Result<JsValue, InputError> {
    let raw_url = property::<JsString>(request.as_ref(), "url")?;
    let constructor = property::<Function>(js_sys::global().as_ref(), "URL")?;
    Reflect::construct(&constructor, &Array::of1(raw_url.as_ref()))
        .map_err(|_| InputError::Unreadable)
}

pub(crate) fn property<T: JsCast>(object: &JsValue, key: &str) -> Result<T, InputError> {
    Reflect::get(object, &JsValue::from_str(key))
        .and_then(JsCast::dyn_into)
        .map_err(|_| InputError::Unreadable)
}

pub(crate) fn is_ascii_literal(value: &JsString, literal: &str) -> bool {
    // `JsString == &str` in the pinned js-sys copies the JS value into Rust.
    value.length() as usize == literal.len() && value.starts_with(literal, 0)
}

fn copy_url_ascii(
    budget: &mut ConstructionBudget,
    value: &JsString,
    start: u32,
) -> Result<String, InputError> {
    // Serialized HTTP(S) paths and queries percent-encode non-ASCII text.
    let mut bytes = budget
        .allocate_exact((value.length() - start) as usize)
        .map_err(|_| CAPACITY)?;
    for i in start..value.length() {
        let code = value.char_code_at(i);
        if !(0.0..=127.0).contains(&code) {
            return Err(InputError::Unreadable);
        }
        bytes.push(code as u8);
    }
    String::from_utf8(bytes).map_err(|_| InputError::Unreadable)
}

fn content_length(value: &JsString) -> Result<usize, InputError> {
    if value.length() == 0 {
        return Err(InputError::Unreadable);
    }
    (0..value.length()).try_fold(0usize, |len, i| {
        let code = value.char_code_at(i);
        if !(48.0..=57.0).contains(&code) {
            return Err(InputError::Unreadable);
        }
        len.checked_mul(10)
            .and_then(|n| n.checked_add(code as usize - 48))
            .filter(|&n| n <= STAGING_SOURCE_BYTES)
            .ok_or(TOO_LARGE)
    })
}

#[cfg(feature = "staging-body-wasm-test")]
mod wasm_test {
    use super::*;
    use worker::wasm_bindgen::{self, prelude::*};

    #[wasm_bindgen]
    pub fn staging_metadata_test(request: web_sys::Request, limit: u32) -> Result<Array, JsValue> {
        let mut budget = ConstructionBudget::new(limit as usize);
        let meta = admit_metadata(&request, &mut budget)
            .map_err(|error| JsValue::from_str(&format!("{error:?}")))?;
        assert_eq!(meta.path.capacity(), meta.path.len());
        assert_eq!(meta.query.capacity(), meta.query.len());
        let declared = match meta.body {
            BodyPlan::Skip => JsValue::UNDEFINED,
            BodyPlan::Form { declared: None } => JsValue::NULL,
            BodyPlan::Form { declared: Some(n) } => JsValue::from(n as u32),
        };
        Ok([
            JsValue::from_str(meta.method),
            JsValue::from_str(&meta.path),
            JsValue::from_str(&meta.query),
            declared,
            JsValue::from(budget.charged() as u32),
        ]
        .into_iter()
        .collect())
    }
}
