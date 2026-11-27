//! Authenticated dispatch for the staging construction scope.

use std::future::Future;

use nntune_subsonic::capped_render::Format;
use nntune_subsonic::envelope::{self, code};
use nntune_subsonic::routes::{route, Route};
use nntune_subsonic::staging_browse::MemberBrowse;
use nntune_subsonic::staging_endpoints::basic_document;
use nntune_subsonic::staging_rejection::StagingRejection::BusyOrCapacity;
use nntune_subsonic::staging_rejection::{RejectionFormat, StagingRejection};
use nntune_subsonic::Params;
use worker::{web_sys, Response, Result as WorkerResult};

use super::{
    InputError, StagingConstruction, StagingInput, SyntheticCatalogue, CONSTRUCTION_BYTES,
};
use crate::postgres::PgClient;
use crate::rotation::KekSource;
use crate::subsonic_auth::{self, AuthError, IndexKeyring, Principal};
use crate::subsonic_dispatch::{snapshot_order::OrderWork, ProductionStore};
use crate::subsonic_http::CredentialParseError;
use crate::subsonic_snapshot_plan::build_owned_bounded_snapshot;
use crate::subsonic_staging_response::{
    finish_document, prepare, respond_fixed_rejection, respond_unreadable,
    respond_unsupported_method, with_request_format, DocumentError, StagingDocument,
    DOCUMENT_BYTES,
};
use crate::worker_entry::{
    load_staging_account,
    subsonic_membership::load_staging_membership,
    subsonic_mirror::{load_staging_mirror, MirrorSelection},
};

pub struct StagingDispatch {
    pub now_millis: i64,
    pub order_work: OrderWork,
    pub construction_headroom: Option<usize>,
    #[cfg(feature = "staging-body-wasm-test")]
    pub retain_document_permit: bool,
}

impl StagingDispatch {
    pub(crate) const FIXTURE_ORDER_WORK: OrderWork = OrderWork {
        // Insertion-sort pairs and mapping lookups for at most four albums,
        // one artist and 1,024 tracks (checked again by the snapshot builder).
        comparisons: 532_000,
        artist_row_probes: 0,
    };
}

pub enum StagingApp<'a, K> {
    Postgres {
        client: PgClient,
        peppers: IndexKeyring<'a>,
        keks: &'a K,
        now_seconds: u64,
    },
    #[cfg(feature = "staging-body-wasm-test")]
    VerifiedFixture,
}

impl<K> StagingApp<'_, K> {
    fn postgres(&mut self) -> &mut PgClient {
        match self {
            Self::Postgres { client, .. } => client,
            #[cfg(feature = "staging-body-wasm-test")]
            Self::VerifiedFixture => panic!("unexpected staging data load"),
        }
    }
}

pub enum CatalogueSource<C> {
    Synthetic(SyntheticCatalogue),
    IsolatedMirror(C),
}

/// The caller must verify staging arming and app/mirror isolation and bound
/// `open_app`. Source selection runs only after real auth, before account,
/// fixture, membership or mirror reads. `None` denies access.
pub async fn respond_staging<'a, K: KekSource + 'a, A, M, C>(
    request: &web_sys::Request,
    context: StagingDispatch,
    open_app: impl FnOnce() -> A,
    authorize_catalogue: impl FnOnce(&Principal) -> Option<CatalogueSource<C>>,
) -> WorkerResult<Response>
where
    A: Future<Output = Result<StagingApp<'a, K>, StagingRejection>>,
    C: FnOnce() -> M,
    M: Future<Output = Result<PgClient, StagingRejection>>,
{
    let reject_xml = |reason| respond_fixed_rejection(reason, RejectionFormat::Xml);
    let input = match StagingInput::collect(request).await {
        Ok(input) => input,
        Err(error) => return error.respond(),
    };
    let mut scope = match StagingConstruction::begin(input) {
        Ok(scope) => scope,
        Err(reason) => return reject_xml(reason),
    };
    let Route::Endpoint(name) = route(scope.path()) else {
        return finish_document(Ok(None));
    };
    if !MemberBrowse::supports(name) && !matches!(name, "ping" | "getLicense" | "getMusicFolders") {
        return finish_document(Ok(None));
    }
    let credentials = match scope.parse_credentials() {
        Ok(credentials) => Ok(credentials),
        Err(CredentialParseError::DuplicateCredential) => Err("duplicate credential parameter"),
        Err(CredentialParseError::MalformedEncoding) => Err("malformed parameter encoding"),
        Err(error) => return InputError::from(error).respond(),
    };
    let params = match scope.dispatch_params() {
        Ok(params) => params,
        Err(error) => return finish_document(Err(error)),
    };
    let (format, refusal, opening) = match with_request_format(&params, || {
        credentials.map(|credentials| (credentials, open_app()))
    }) {
        Ok(formats) => formats,
        Err(error) => return finish_document(Err(error)),
    };
    let (credentials, opening) = match opening {
        Ok(opening) => opening,
        Err(message) => return scope.protocol_failure(refusal, code::GENERIC, message),
    };
    let mut app = match opening.await {
        Ok(app) => app,
        Err(reason) => return reject_xml(reason),
    };
    let authenticated = match &mut app {
        StagingApp::Postgres {
            client,
            peppers,
            keks,
            now_seconds,
        } => {
            subsonic_auth::authenticate_staging(
                client,
                &credentials,
                peppers,
                *keks,
                *now_seconds,
                &mut scope.input.budget,
            )
            .await
        }
        #[cfg(feature = "staging-body-wasm-test")]
        StagingApp::VerifiedFixture => Ok(Principal::staging_test_fixture()),
    };
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
                    return reject_xml(StagingRejection::Unavailable);
                }
            };
            return scope.protocol_failure(refusal, error.subsonic_code(), message);
        }
    };
    let Some(catalogue) = authorize_catalogue(&principal) else {
        return scope.protocol_failure(
            refusal,
            code::NOT_AUTHORIZED,
            "Not authorized for staging.",
        );
    };
    #[cfg(feature = "staging-body-wasm-test")]
    let retain_document_permit = context.retain_document_permit;
    let document = scope
        .dispatch_document(
            &mut app,
            &principal,
            catalogue,
            name,
            context,
            (&params, format, refusal),
        )
        .await;
    #[cfg(feature = "staging-body-wasm-test")]
    if retain_document_permit {
        return crate::subsonic_staging_response::finish_streamed_document(
            document,
            &mut scope.input.budget,
            scope._permit,
        );
    }
    finish_document(document.map(Some))
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
    fn dispatch_params(&mut self) -> Result<Params, DocumentError> {
        let form = (self.method() == "POST").then_some(self.input.form.as_str());
        Params::parse_charged(&self.input.query, form, &mut self.input.budget)
            .map_err(|_| DocumentError::Rejected(BusyOrCapacity, RejectionFormat::Xml))
    }

    fn protocol_failure(
        mut self,
        refusal: RejectionFormat,
        err_code: i64,
        message: &str,
    ) -> WorkerResult<Response> {
        let format = match refusal {
            RejectionFormat::Json => Format::Json,
            RejectionFormat::Xml => Format::Xml,
        };
        let budget = &mut self.input.budget;
        let document = envelope::charged_error_document(budget, err_code, message)
            .map_err(|_| DocumentError::Rejected(BusyOrCapacity, refusal))
            .and_then(|graph| prepare(budget, &graph, format, 1024));
        finish_document(document.map(Some))
    }

    async fn dispatch_document<F: Future<Output = Result<PgClient, StagingRejection>>>(
        &mut self,
        app: &mut StagingApp<'_, impl KekSource>,
        principal: &Principal,
        catalogue: CatalogueSource<impl FnOnce() -> F>,
        name: &str,
        context: StagingDispatch,
        representation: (&Params, Format<'_>, RejectionFormat),
    ) -> Result<StagingDocument, DocumentError> {
        let (params, format, refusal) = representation;
        let reject = |reason| DocumentError::Rejected(reason, refusal);
        if let Some(headroom) = context.construction_headroom {
            if !matches!(&catalogue, CatalogueSource::Synthetic(_))
                || name != "getArtists"
                || !matches!(format, Format::Json)
            {
                return Err(reject(StagingRejection::Unavailable));
            }
            self.input.budget.restrict_remaining(headroom);
        }
        let budget = &mut self.input.budget;
        let doc = if MemberBrowse::supports(name) {
            let (artists, releases, tracks) = match catalogue {
                CatalogueSource::Synthetic(fixture) => {
                    fixture.rows(budget).map_err(|_| reject(BusyOrCapacity))?
                }
                CatalogueSource::IsolatedMirror(connect_isolated) => {
                    let member = load_staging_membership(budget, app.postgres(), principal)
                        .await
                        .map_err(reject)?;
                    let selection = MirrorSelection {
                        releases: &member.releases,
                        pairs: &member.pairs,
                        remaining_text_bytes: member.remaining_text_bytes,
                    };
                    load_staging_mirror(budget, selection, connect_isolated)
                        .await
                        .map_err(reject)?
                }
            };
            let (artists, albums, songs) = build_owned_bounded_snapshot(
                budget,
                &artists,
                &releases,
                &tracks,
                context.order_work,
                8 * 1024,
                CONSTRUCTION_BYTES,
            )
            .map_err(|_| reject(BusyOrCapacity))?;
            MemberBrowse {
                artists: &artists,
                albums: &albums,
                songs: &songs,
                now_millis: context.now_millis,
            }
            .document(budget, name, params, |id| {
                ProductionStore::resolve_song_occurrence(&tracks, &songs, id)
            })
        } else if name == "getLicense" {
            let (identity, _) = load_staging_account(budget, app.postgres(), principal)
                .await
                .map_err(reject)?;
            basic_document(budget, name, &identity.email)
        } else {
            basic_document(budget, name, "")
        }
        .map_err(|_| reject(BusyOrCapacity))?
        .ok_or_else(|| reject(BusyOrCapacity))?;
        prepare(budget, &doc, format, DOCUMENT_BYTES)
    }
}

#[cfg(feature = "staging-body-wasm-test")]
mod wasm_test {
    use super::*;
    use crate::handlers::Config;
    use crate::worker_entry::with_deadline;
    use worker::wasm_bindgen::{self, prelude::*};
    use worker::{js_sys, wasm_bindgen_futures::JsFuture};

    #[wasm_bindgen]
    pub async fn staging_dispatch_test(
        request: web_sys::Request,
        open_app: js_sys::Function,
        allowed: bool,
        fixture_id: Option<String>,
        retain_document_permit: Option<bool>,
        construction_headroom: Option<u32>,
    ) -> Result<web_sys::Response, JsValue> {
        type NoMirror = fn() -> std::future::Ready<Result<PgClient, StagingRejection>>;
        let context = StagingDispatch {
            now_millis: 0,
            order_work: StagingDispatch::FIXTURE_ORDER_WORK,
            construction_headroom: construction_headroom.map(|bytes| bytes as usize),
            retain_document_permit: retain_document_permit.unwrap_or(false),
        };
        let fixture = SyntheticCatalogue::from_trusted_id(fixture_id.as_deref().unwrap_or("empty"))
            .ok_or(JsValue::NULL)?;
        respond_staging(
            &request,
            context,
            || {
                with_deadline(
                    async {
                        let ready = open_app.call0(&JsValue::UNDEFINED).unwrap();
                        let result = JsFuture::from(js_sys::Promise::resolve(&ready))
                            .await
                            .unwrap();
                        (result.as_bool() == Some(true))
                            .then_some(StagingApp::<Config>::VerifiedFixture)
                            .ok_or(StagingRejection::Unavailable)
                    },
                    2_000,
                    BusyOrCapacity,
                )
            },
            |principal| {
                assert_eq!(principal.user_id(), "fixture-user");
                allowed.then_some(CatalogueSource::<NoMirror>::Synthetic(fixture))
            },
        )
        .await
        .map(Into::into)
        .map_err(|_| JsValue::NULL)
    }
}
