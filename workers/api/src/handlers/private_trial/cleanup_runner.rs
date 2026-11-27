//! Entry point of the one-shot Worker built by the protected staging workflow.

use std::time::{Duration, UNIX_EPOCH};

use nntune_core::s3_locator::{S3Binding, S3Locator};
use nntune_ingest::multipart::s3_completion::FixtureVersionRemover;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use tokio_postgres::{
    config::{Host, SslMode},
    types::Type,
    Client, Config,
};
use worker::{Env, Method, Request, Response, SecureTransport, Socket};
use zeroize::Zeroizing;

use super::{
    caller::FIXTURE_DIGEST,
    cleanup::{cleanup_with_s3, Fixture},
    parse_object,
    TrialError::{self, Refused, Unconfirmed},
    FIXTURE_BUCKET as BUCKET,
};
use crate::{
    storage_digest_read::uuid_text,
    worker_entry::{
        now_millis,
        private_browser_worker::{hex, uuid_v4},
        with_deadline,
    },
};

const DATABASE: &str = "nntunev2_staging";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Scope {
    provider: Provider,
    database_host: String,
    database_port: u16,
    database_role: String,
    database_role_oid: u32,
    s3_credential_sha256: String,
    object: String,
    generation: String,
    map: String,
    track: String,
    session: String,
    job: String,
    issuer: String,
    invite: i64,
    cutoff: i64,
    media_version: String,
    page_version: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Approval {
    provider: Provider,
    scope_sha256: String,
    s3_access_key_sha256: String,
}

#[derive(Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct Provider {
    environment: String,
    library: String,
    configuration: String,
    root_version: String,
    bucket: String,
    endpoint: String,
    region: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct S3Credential {
    access_key: String,
    secret_key: String,
}

fn secret(env: &Env, name: &str, limit: usize) -> Result<Zeroizing<String>, TrialError> {
    let value = Zeroizing::new(env.secret(name).map_err(|_| Refused)?.to_string());
    (!value.is_empty() && value.len() <= limit)
        .then_some(value)
        .ok_or(Refused)
}

fn match_sha256(raw: &str, expected: &str) -> Result<(), TrialError> {
    let expected = hex::<32>(expected).ok_or(Refused)?;
    bool::from(Sha256::digest(raw.as_bytes())[..].ct_eq(&expected))
        .then_some(())
        .ok_or(Refused)
}

fn uuid(raw: &str) -> Result<[u8; 16], TrialError> {
    uuid_v4(raw).ok_or(Refused)
}

async fn connect(scope: &Scope, dsn: &str) -> Result<Client, TrialError> {
    let cfg: Config = dsn.parse().map_err(|_| Refused)?;
    if cfg.get_dbname() != Some(DATABASE)
        || cfg.get_user() != Some(&scope.database_role)
        || cfg.get_hosts() != [Host::Tcp(scope.database_host.clone())]
        || cfg.get_ports() != [scope.database_port]
        || !cfg.get_hostaddrs().is_empty()
        || cfg.get_options().is_some()
        || cfg.get_password().is_none_or(|pw| pw.is_empty())
        || scope.database_host.is_empty()
        || scope.database_port == 0
        || scope.database_role.is_empty()
        || scope.database_role_oid == 0
        || cfg.get_ssl_mode() != SslMode::Require
    {
        return Err(Refused);
    }
    let socket = Socket::builder()
        .secure_transport(SecureTransport::StartTls)
        .connect(&scope.database_host, scope.database_port)
        .map_err(|_| Unconfirmed)?;
    socket.opened().await.map_err(|_| Unconfirmed)?;
    let (db, connection) = cfg
        .connect_raw(socket, worker::postgres_tls::PassthroughTls)
        .await
        .map_err(|_| Unconfirmed)?;
    wasm_bindgen_futures::spawn_local(async move {
        let _ = connection.await;
    });
    db.batch_execute("SET statement_timeout = '3s'; SET lock_timeout = '2s'")
        .await
        .map_err(|_| Unconfirmed)?;
    let verified = db.query_typed_opt(
        "SELECT 1 FROM pg_catalog.pg_roles r
         WHERE r.rolname = current_user AND session_user = current_user
           AND r.rolname::text = $1 AND r.oid = $2 AND current_database() = 'nntunev2_staging'
           AND r.rolcanlogin AND NOT (r.rolsuper OR r.rolbypassrls OR r.rolcreatedb OR r.rolcreaterole OR r.rolreplication)
           AND NOT has_any_column_privilege(current_user, 'public.users', 'SELECT')
           AND NOT has_any_column_privilege(current_user, 'public.private_library_admissions', 'INSERT')
           AND NOT has_any_column_privilege(current_user, 'public.storage_object_intents', 'INSERT')",
        &[(&scope.database_role, Type::TEXT), (&scope.database_role_oid, Type::OID)],
    ).await.map_err(|_| Unconfirmed)?;
    verified.ok_or(Refused)?;
    Ok(db)
}

async fn run(env: &Env) -> Result<bool, TrialError> {
    if env
        .var("CLEANUP_STAGE")
        .ok()
        .map(|v| v.to_string())
        .as_deref()
        != Some("staging")
    {
        return Err(Refused);
    }
    let raw = secret(env, "CLEANUP_SCOPE", 8_192)?;
    let approval: Approval = parse_object(&secret(env, "CLEANUP_APPROVAL", 2_048)?, 2_048)?;
    let dispatch_scope = secret(env, "CLEANUP_SCOPE_SHA256", 64)?;
    if approval.scope_sha256.as_str() != dispatch_scope.as_str() {
        return Err(Refused);
    }
    match_sha256(&raw, &approval.scope_sha256)?;
    let scope: Scope = parse_object(&raw, 8_192)?;
    let provider = approval.provider;
    if scope.provider != provider
        || provider.bucket != BUCKET
        || scope.invite <= 0
        || scope.cutoff <= 0
        || scope.cutoff as u64 > now_millis() / 1_000
        || !nntune_ingest::jobs::validate_job_id(&scope.job)
        || !(1..=128).contains(&scope.issuer.len())
        || scope.issuer.chars().any(char::is_control)
    {
        return Err(Refused);
    }
    for id in [
        &scope.map,
        &scope.track,
        &scope.session,
        &provider.root_version,
    ] {
        uuid(id)?;
    }
    let fixture = Fixture {
        environment: uuid(&provider.environment)?,
        library: uuid(&provider.library)?,
        object: uuid(&scope.object)?,
        track: &scope.track,
        session: &scope.session,
        job: &scope.job,
        issuer: &scope.issuer,
        invite: scope.invite,
        bucket: BUCKET,
        cutoff: scope.cutoff,
    };
    let key = format!(
        "media/{}/{}",
        uuid_text(fixture.object),
        uuid_text(uuid(&scope.generation)?)
    );
    for version in [&scope.media_version, &scope.page_version] {
        S3Locator::new(
            BUCKET.into(),
            key.clone(),
            S3Binding::VersionId(version.clone()),
        )
        .map_err(|_| Refused)?;
    }
    let credential = secret(env, "CLEANUP_S3", 2_048)?;
    match_sha256(&credential, &scope.s3_credential_sha256)?;
    let keys: S3Credential = parse_object(&credential, 2_048)?;
    match_sha256(&keys.access_key, &approval.s3_access_key_sha256)?;
    let remover = FixtureVersionRemover::new(
        uuid(&provider.configuration)?,
        &provider.endpoint,
        provider.region.clone(),
        keys.access_key,
        keys.secret_key,
    )
    .map_err(|_| Refused)?;
    let mut db = connect(&scope, &secret(env, "CLEANUP_DATABASE_URL", 2_048)?).await?;
    // These storage identities and pins are append-only. The adapter takes
    // the retirement locks and checks the mutable admission, reference and lease state.
    db.query_typed_opt(
        include_str!("cleanup_runner.sql"),
        &[
            (&provider.environment, Type::TEXT),
            (&provider.library, Type::TEXT),
            (&scope.object, Type::TEXT),
            (&scope.generation, Type::TEXT),
            (&scope.map, Type::TEXT),
            (&provider.configuration, Type::TEXT),
            (&provider.root_version, Type::TEXT),
            (&scope.media_version, Type::TEXT),
            (&scope.page_version, Type::TEXT),
            (&scope.track, Type::TEXT),
            (&scope.session, Type::TEXT),
            (&scope.job, Type::TEXT),
            (&scope.issuer, Type::TEXT),
            (&scope.invite, Type::INT8),
            (&BUCKET, Type::TEXT),
            (&FIXTURE_DIGEST.as_slice(), Type::BYTEA),
        ],
    )
    .await
    .map_err(|_| Unconfirmed)?
    .ok_or(Refused)?;
    let until = UNIX_EPOCH + Duration::from_millis(now_millis()) + Duration::from_secs(12);
    cleanup_with_s3(&mut db, &fixture, &remover, until).await
}

#[worker::event(fetch)]
pub async fn cleanup_entry(req: Request, env: Env, _: worker::Context) -> worker::Result<Response> {
    if req.method() != Method::Post
        || req.path() != "/"
        || req
            .headers()
            .get("x-cleanup-invocation")?
            .is_none_or(|token| {
                !secret(&env, "CLEANUP_INVOCATION", 64)
                    .is_ok_and(|pin| bool::from(pin.as_bytes().ct_eq(token.as_bytes())))
            })
    {
        return Response::error("refused", 403);
    }
    match with_deadline(run(&env), 25_000, Unconfirmed).await {
        Ok(true) => Response::ok("confirmed"),
        Ok(false) => Response::error("withheld", 409),
        Err(Refused) => Response::error("refused", 403),
        Err(Unconfirmed) => Response::error("unconfirmed", 503),
    }
}
