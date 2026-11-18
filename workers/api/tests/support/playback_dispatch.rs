//! Guarded native dispatcher fixture. Use `with_playback_account` for a
//! committed entitled account and an independent observer. Send real toggle,
//! issuance, refusal and heartbeat requests via `dispatch` and `request`;
//! for faults, supply a `playback_store_for_postgres_test` connector that
//! returns the faulted client only at the intended checkout.
use super::{clock, dispatch, dispatcher_config, disposable_accounts, token};
use futures_util::FutureExt;
use nntune_api::auth::SingleUseStore;
use nntune_api::handlers::{Config, Request, Store, StoreError};
use nntune_api::ledger::{derive_play_idempotency_key, period_month_for_timestamp};
use nntune_api::meter_sink::SINK_PATH;
use nntune_api::session::{SessionClaims, COOKIE_NAME};
use nntune_api::worker_entry::playback_store_for_postgres_test;
use serde_json::{json, Value};
use std::io::Write;
use std::panic::{resume_unwind, AssertUnwindSafe};
use std::path::Path;
use std::process::{Command, Stdio};
use tokio_postgres::Client;

pub(super) const TRACK: &str = "track-1";
const GRANT_KEY: &str = "synthetic-dispatch-play-grant-key";
const METER_KEY: &str = "synthetic-dispatch-meter-sink-key";

pub(super) struct PlaybackAccount<'a> {
    pub observer: &'a Client,
    pub user: &'a str,
    pub cfg: Config,
    pub now: u64,
    cookie: String,
}

pub(super) async fn playback_client() -> Result<Client, StoreError> {
    let client = disposable_accounts::connect_guarded()
        .await
        .map_err(|_| StoreError)?;
    client
        .batch_execute("SET search_path TO public")
        .await
        .map_err(|_| StoreError)?;
    Ok(client)
}

pub(super) fn playback_store() -> impl Store + SingleUseStore {
    playback_store_for_postgres_test(playback_client)
}

pub(super) async fn with_playback_account(test: impl AsyncFnOnce(&PlaybackAccount<'_>)) {
    let accounts = disposable_accounts::Accounts::new(1).await;
    let result = AssertUnwindSafe(async {
        let user = accounts.id(0);
        let observer = &accounts.observer;
        let now = clock(observer).await;
        let session_id = format!("session-{user}");
        observer.execute(
            "INSERT INTO public.sessions (id,user_id,created_at,expires_at) VALUES ($1,$2,$3,$4)",
            &[&session_id, &user, &now, &(now + 3600)],
        ).await.expect("seed account session");
        observer.execute(
            "INSERT INTO public.subscriptions (user_id,stripe_subscription_id,stripe_customer_id,stripe_subscription_item_id,stripe_price_id,plan_id,status,paid_through_at) VALUES ($1,$1,$1,$1,'synthetic-price','plus','active',now() + interval '1 hour')",
            &[&user],
        ).await.expect("seed entitlement");
        let cfg = dispatcher_config()
            .with_play_grant_secret(GRANT_KEY.as_bytes().to_vec())
            .with_meter_sink_secret(METER_KEY.as_bytes().to_vec());
        let now = now as u64;
        let signed = token(&SessionClaims {
            session_id,
            user_id: user.into(),
            created_at: now,
            issued_at: now,
            expires_at: now + 3600,
            rotation_counter: 0,
        });
        test(&PlaybackAccount { observer, user, cfg, now, cookie: format!("{COOKIE_NAME}={signed}") }).await;
    }).catch_unwind().await;
    accounts.cleanup().await;
    if let Err(panic) = result {
        resume_unwind(panic);
    }
}

fn allows_stream_build(actions: Option<&str>, runner: Option<&str>, host_scratch: bool) -> bool {
    actions == Some("true") && runner == Some("github-hosted") && !host_scratch
}

impl PlaybackAccount<'_> {
    pub fn request(&self, method: &str, path: &str, body: Value) -> Request {
        Request {
            method: method.into(),
            path: path.into(),
            cookie: Some(self.cookie.clone()),
            origin: Some(self.cfg.origin.clone()),
            authorization: None,
            stripe_signature: None,
            body: body.to_string().into_bytes(),
        }
    }

    /// Feed the issued grant and its identity unchanged to the native stream
    /// test; return the stream's exact signed body as the real sink request.
    /// The nested build is only supported on GitHub-hosted Actions.
    pub fn delivered_batch(&self, issued: &Value) -> Request {
        // The local Cargo pool assigns this scratch selector alongside its
        // leased target, which the nested, scrubbed build would not inherit.
        assert!(
            allows_stream_build(
                std::env::var("GITHUB_ACTIONS").ok().as_deref(),
                std::env::var("RUNNER_ENVIRONMENT").ok().as_deref(),
                std::env::var_os("PAPERCLIP_RUN_SCRATCH_DIR").is_some(),
            ),
            "the stream delivery fixture requires GitHub-hosted Actions (api-migrations); no nested Cargo build was started"
        );
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        // The stream and API have separate locked dependency graphs and target
        // directories. Use the compiler beside the invoking Cargo so parallel
        // rustc calls don't run through rustup after scrubbing the environment.
        // Pass no DB URL, provider settings, or credentials.
        let rustc = Path::new(env!("CARGO")).with_file_name("rustc");
        let mut child = Command::new("timeout")
            .args([
                "-k",
                "5s",
                "180s",
                env!("CARGO"),
                "test",
                "--locked",
                "--quiet",
                "-p",
                "nntune-stream",
                "--test",
                "play_route_acceptance",
                "--",
                "--exact",
                "--ignored",
                "--nocapture",
                "emit_meter_batch_for_issued_grant",
            ])
            .current_dir(workspace)
            .env_clear()
            .envs(
                ["PATH", "HOME", "CARGO_HOME", "RUSTUP_HOME"]
                    .into_iter()
                    .filter_map(|key| std::env::var_os(key).map(|value| (key, value))),
            )
            .env("RUSTC", rustc)
            .env("CARGO_PROFILE_TEST_DEBUG", "0")
            .env("CARGO_INCREMENTAL", "0")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("start locked stream fixture");
        let input = json!([
            issued["grant"],
            self.user,
            issued["play_id"],
            self.now,
            GRANT_KEY,
            METER_KEY
        ]);
        let wrote = child
            .stdin
            .take()
            .unwrap()
            .write_all(input.to_string().as_bytes());
        let output = child.wait_with_output().expect("reap stream fixture");
        assert!(
            output.status.success(),
            "stream fixture failed: {}",
            output.status
        );
        wrote.expect("send issued grant to stream fixture");
        let stdout = std::str::from_utf8(&output.stdout).expect("stream fixture output");
        let mut batches = stdout
            .lines()
            .filter_map(|line| line.strip_prefix("STREAM_METER_BATCH "));
        let (auth, body): (String, String) =
            serde_json::from_str(batches.next().expect("stream fixture did not emit a batch"))
                .expect("stream batch envelope");
        assert!(
            batches.next().is_none(),
            "expected exactly one signed batch"
        );
        Request {
            method: "POST".into(),
            path: SINK_PATH.into(),
            cookie: None,
            origin: None,
            authorization: Some(auth),
            stripe_signature: None,
            body: body.into_bytes(),
        }
    }
}

#[test]
fn stream_fixture_requires_hosted_actions_without_host_scratch() {
    let hosted = Some("github-hosted");
    assert!(allows_stream_build(Some("true"), hosted, false));
    for (actions, runner, host_scratch) in [
        (None, None, false),
        (None, hosted, false),
        (Some("false"), hosted, false),
        (Some("true"), None, false),
        (Some("true"), Some("self-hosted"), false),
        (Some("true"), hosted, true),
    ] {
        assert!(!allows_stream_build(actions, runner, host_scratch));
    }
}

#[tokio::test]
async fn issued_public_grant_bills_delivered_union_but_unused_grant_does_not() {
    with_playback_account(async |fx| {
        let store = playback_store();
        let new_play = fx.request("POST", "/v1/plays", json!({"track_id": TRACK, "session_id": "player"}));
        let issued = dispatch(&store, &fx.cfg, fx.now, &new_play).await;
        assert_eq!(issued.status, 200);
        let issued: Value = serde_json::from_str(&issued.body).unwrap();
        let play_id = issued["play_id"].as_str().unwrap();
        let batch = fx.delivered_batch(&issued);
        let ack = dispatch(&store, &fx.cfg, fx.now, &batch).await;
        assert_eq!(ack.status, 200);
        assert_eq!(serde_json::from_str::<Value>(&ack.body).unwrap(), json!({"accepted": [play_id], "fenced": [], "rejected": []}));

        let unused = dispatch(&store, &fx.cfg, fx.now, &new_play).await;
        assert_eq!(unused.status, 200);
        let unused: Value = serde_json::from_str(&unused.body).unwrap();
        assert!(unused["grant"].as_str().is_some_and(|token| !token.is_empty()));
        assert_ne!(unused["play_id"].as_str().unwrap(), play_id);

        // Only the first grant passed through the stream worker. The second
        // must add neither a per-play row nor an increment to the plan meter.
        let key = derive_play_idempotency_key(fx.user, TRACK, play_id).unwrap();
        let rows = fx.observer.query(
            "SELECT idempotency_key, track_id, period_month, bytes_served, stream_seconds FROM public.stream_play_ledger WHERE user_id = $1",
            &[&fx.user],
        ).await.unwrap();
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(row.get::<_, String>(0), key);
        assert_eq!(row.get::<_, String>(1), TRACK);
        let month = period_month_for_timestamp(fx.now);
        assert_eq!(row.get::<_, String>(2), month);
        assert_eq!(row.get::<_, i64>(3), 80_000);
        assert_eq!(row.get::<_, i32>(4), 2);
        assert_eq!(store.monthly_stream_seconds(fx.user, &month).await.unwrap(), 2);
    }).await;
}
