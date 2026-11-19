//! Authenticated dispatch for the unmounted staging construction scope.

use std::future::Future;

use nntune_subsonic::capped_render::Format;
use nntune_subsonic::construction_budget::{BudgetExceeded, ConstructionBudget};
use nntune_subsonic::envelope::{self, code};
use nntune_subsonic::routes::{route, Route};
use nntune_subsonic::staging_browse::MemberBrowse;
use nntune_subsonic::staging_endpoints::basic_document;
use nntune_subsonic::staging_rejection::StagingRejection::BusyOrCapacity;
use nntune_subsonic::staging_rejection::{RejectionFormat, StagingRejection};
use nntune_subsonic::Params;
use tokio_postgres::Client;
use worker::{web_sys, Response, Result as WorkerResult};

use super::{InputError, StagingConstruction, StagingInput, CONSTRUCTION_BYTES};
use crate::rotation::KekSource;
use crate::subsonic_auth::{self, AuthError, IndexKeyring, Principal};
use crate::subsonic_dispatch::{snapshot_order::OrderWork, ProductionStore};
use crate::subsonic_http::CredentialParseError;
use crate::subsonic_snapshot_plan::build_owned_bounded_snapshot;
use crate::subsonic_staging_response::{
    finish_document, prepare, respond_fixed_rejection, respond_unreadable,
    respond_unsupported_method, DocumentError, StagingDocument, DOCUMENT_BYTES,
};
use crate::worker_entry::{
    load_staging_account,
    subsonic_membership::load_staging_membership,
    subsonic_mirror::{load_staging_mirror, MirrorSelection},
    with_deadline,
};

pub struct StagingDispatch<'a, K> {
    pub peppers: &'a IndexKeyring<'a>,
    pub keks: &'a K,
    pub now_seconds: u64,
    pub now_millis: i64,
    pub order_work: OrderWork,
}

/// Unmounted. The caller must first verify staging arming and app/mirror
/// isolation. The supplied fixture authorization runs after real auth and
/// before account, membership or mirror reads.
pub async fn respond_staging<K: KekSource, A, M>(
    request: &web_sys::Request,
    context: StagingDispatch<'_, K>,
    open_app: impl FnOnce() -> A,
    authorize_fixture: impl FnOnce(&Principal) -> bool,
    open_mirror: impl FnOnce() -> M,
) -> WorkerResult<Response>
where
    A: Future<Output = Result<Client, StagingRejection>>,
    M: Future<Output = Result<Client, StagingRejection>>,
{
    let input = match StagingInput::collect(request).await {
        Ok(input) => input,
        Err(error) => return error.respond(),
    };
    let mut scope = match StagingConstruction::begin(input) {
        Ok(scope) => scope,
        Err(reason) => return respond_fixed_rejection(reason, RejectionFormat::Xml),
    };
    let Route::Endpoint(name) = route(scope.path()) else {
        return finish_document(Ok(None));
    };
    if !MemberBrowse::supports(name) && !matches!(name, "ping" | "getLicense" | "getMusicFolders") {
        return finish_document(Ok(None));
    }
    let credentials = match scope.parse_credentials() {
        Ok(credentials) => credentials,
        Err(CredentialParseError::DuplicateCredential) => {
            return scope.protocol_failure(code::GENERIC, "duplicate credential parameter");
        }
        Err(CredentialParseError::MalformedEncoding) => {
            return scope.protocol_failure(code::GENERIC, "malformed parameter encoding");
        }
        Err(error) => return InputError::from(error).respond(),
    };
    let opened = with_deadline(async { open_app().await }, 2_000, BusyOrCapacity);
    let mut app = match opened.await {
        Ok(app) => app,
        Err(reason) => return respond_fixed_rejection(reason, RejectionFormat::Xml),
    };
    let authenticated = subsonic_auth::authenticate_staging(
        &mut app,
        &credentials,
        context.peppers,
        context.keks,
        context.now_seconds,
        scope.budget(),
    )
    .await;
    drop(credentials);
    let principal = match authenticated {
        Ok(principal) => principal,
        Err(error) => {
            let message = match error {
                AuthError::Unsupported => "Provided authentication mechanism not supported.",
                AuthError::Conflicting => "Conflicting authentication parameters.",
                AuthError::MissingSalt => "Required parameter is missing.",
                AuthError::InvalidKey => "Invalid API key.",
                AuthError::InvalidToken => "Wrong username or password.",
                AuthError::Unavailable => {
                    return respond_fixed_rejection(
                        StagingRejection::Unavailable,
                        RejectionFormat::Xml,
                    );
                }
            };
            return scope.protocol_failure(error.subsonic_code(), message);
        }
    };
    if !authorize_fixture(&principal) {
        return scope.protocol_failure(code::NOT_AUTHORIZED, "Not authorized for staging.");
    }
    let document = scope
        .dispatch_document(
            &mut app,
            &principal,
            open_mirror,
            name,
            context.order_work,
            context.now_millis,
        )
        .await;
    finish_document(document)
}

impl InputError {
    fn respond(self) -> WorkerResult<Response> {
        match self {
            Self::Rejected(reason) => respond_fixed_rejection(reason, RejectionFormat::Xml),
            Self::UnsupportedMethod => respond_unsupported_method(),
            Self::Unreadable => respond_unreadable(),
        }
    }
}

impl StagingConstruction {
    fn budget(&mut self) -> &mut ConstructionBudget {
        &mut self.input.budget
    }

    // StagingInput can only be collected after validating the raw sources.
    fn dispatch_params(&mut self) -> Result<Params, BudgetExceeded> {
        let form = (self.method() == "POST").then_some(self.input.form.as_str());
        Params::parse_charged(&self.input.query, form, &mut self.input.budget)
    }

    fn protocol_failure(mut self, err_code: i64, message: &str) -> WorkerResult<Response> {
        let document = (|| {
            let params = self
                .dispatch_params()
                .map_err(|_| DocumentError::Rejected(BusyOrCapacity, RejectionFormat::Xml))?;
            let selector = params.get("f");
            let refusal = RejectionFormat::from_safe_selector(selector);
            if selector == Some("jsonp")
                && !params
                    .get("callback")
                    .is_some_and(envelope::is_valid_jsonp_callback)
            {
                return Err(DocumentError::InvalidCallback);
            }
            let format = match refusal {
                RejectionFormat::Json => Format::Json,
                RejectionFormat::Xml => Format::Xml,
            };
            let graph = envelope::charged_error_document(self.budget(), err_code, message)
                .map_err(|_| DocumentError::Rejected(BusyOrCapacity, refusal))?;
            prepare(self.budget(), &graph, format, 1024).map(Some)
        })();
        finish_document(document)
    }

    async fn dispatch_document<F>(
        &mut self,
        client: &mut Client,
        principal: &Principal,
        connect_isolated: impl FnOnce() -> F,
        name: &str,
        work: OrderWork,
        now_millis: i64,
    ) -> Result<Option<StagingDocument>, DocumentError>
    where
        F: Future<Output = Result<Client, StagingRejection>>,
    {
        let params = self
            .dispatch_params()
            .map_err(|_| DocumentError::Rejected(BusyOrCapacity, RejectionFormat::Xml))?;
        let selector = params.get("f");
        let refusal = RejectionFormat::from_safe_selector(selector);
        let reject = |reason| DocumentError::Rejected(reason, refusal);
        let format = match selector {
            Some("json") => Format::Json,
            Some("jsonp") => match params
                .get("callback")
                .filter(|cb| envelope::is_valid_jsonp_callback(cb))
            {
                Some(callback) => Format::Jsonp(callback),
                None => return Err(DocumentError::InvalidCallback),
            },
            _ => Format::Xml,
        };
        let doc = if MemberBrowse::supports(name) {
            let member = load_staging_membership(self.budget(), client, principal)
                .await
                .map_err(reject)?;
            let (artists, releases, tracks) = load_staging_mirror(
                self.budget(),
                MirrorSelection {
                    releases: &member.releases,
                    pairs: &member.pairs,
                    remaining_text_bytes: member.remaining_text_bytes,
                },
                connect_isolated,
            )
            .await
            .map_err(reject)?;
            drop(member);
            let (artists, albums, songs) = build_owned_bounded_snapshot(
                self.budget(),
                &artists,
                &releases,
                &tracks,
                work,
                8 * 1024,
                CONSTRUCTION_BYTES,
            )
            .map_err(|_| reject(BusyOrCapacity))?;
            MemberBrowse {
                artists: &artists,
                albums: &albums,
                songs: &songs,
                now_millis,
            }
            .document(self.budget(), name, &params, |id| {
                ProductionStore::resolve_song_occurrence(&tracks, &songs, id)
            })
        } else if name == "getLicense" {
            let (identity, _) = load_staging_account(self.budget(), client, principal)
                .await
                .map_err(reject)?;
            basic_document(self.budget(), name, &identity.email)
        } else {
            basic_document(self.budget(), name, "")
        }
        .map_err(|_| reject(BusyOrCapacity))?
        .ok_or_else(|| reject(BusyOrCapacity))?;
        prepare(self.budget(), &doc, format, DOCUMENT_BYTES).map(Some)
    }
}

#[cfg(feature = "staging-body-wasm-test")]
mod wasm_test {
    use super::*;
    use crate::native_rest::Pepper;
    use crate::subsonic_dispatch::snapshot_order::order_work;
    use worker::wasm_bindgen::{self, prelude::*};
    use worker::{js_sys, wasm_bindgen_futures::JsFuture};

    struct NoKeys;

    impl KekSource for NoKeys {
        fn kek(&self, _: u32) -> Option<&[u8; 32]> {
            None
        }
    }

    #[wasm_bindgen]
    pub async fn staging_dispatch_test(
        request: web_sys::Request,
        open_app: js_sys::Function,
    ) -> Result<web_sys::Response, JsValue> {
        let peppers = IndexKeyring::new(
            Pepper {
                version: 1,
                bytes: &[0; 32],
            },
            None,
        )
        .unwrap();
        let context = StagingDispatch {
            peppers: &peppers,
            keks: &NoKeys,
            now_seconds: 0,
            now_millis: 0,
            order_work: order_work(0, 0, 0, 0).unwrap(),
        };
        respond_staging(
            &request,
            context,
            || async {
                let ready = open_app.call0(&JsValue::UNDEFINED).unwrap();
                JsFuture::from(js_sys::Promise::resolve(&ready))
                    .await
                    .unwrap();
                Err(StagingRejection::Unavailable)
            },
            |_| panic!("no principal from unavailable app"),
            || async { panic!("unexpected mirror") },
        )
        .await
        .map(Into::into)
        .map_err(|_| JsValue::NULL)
    }
}
