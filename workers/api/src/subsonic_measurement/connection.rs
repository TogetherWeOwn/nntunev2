use std::cell::{Cell, OnceCell, RefCell};
use std::future::{poll_fn, Future, Ready};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::Poll;

use futures_util::future::{select, Either};
use tokio_postgres::{Client, Connection};

use super::*;
use crate::billing_owned_socket::{SocketGuard, TrackedSocket};
use crate::subsonic_dispatch::snapshot_order::order_work;
use crate::subsonic_staging::{respond_staging, CatalogueSource, StagingApp, StagingDispatch};
use crate::worker_entry::config_from_env;

// Per-isolate DB admission stays busy until socket close is confirmed.
static DB_BUSY: AtomicBool = AtomicBool::new(false);

type Driver = Connection<TrackedSocket, worker::Socket>;
type NoMirror = fn() -> Ready<Result<Client, StagingRejection>>;

pub(super) async fn respond(
    request: &web_sys::Request,
    env: &Env,
    lease: Lease<'_>,
    fixture: SyntheticCatalogue,
    now_ms: u64,
) -> WorkerResult<Response> {
    let socket = SocketGuard::default();
    let driver = RefCell::new(None::<Pin<Box<Driver>>>);
    let acquired = Cell::new(false);
    let config = OnceCell::new();
    let context = StagingDispatch {
        now_millis: now_ms as i64,
        order_work: order_work(1, 1, 1, 1).expect("single-row fixture order work"),
    };
    let response = {
        let operation = within_request(
            respond_staging(
                request,
                context,
                || async {
                    let cfg = config
                        .get_or_init(|| config_from_env(env))
                        .as_ref()
                        .ok_or(Unavailable)?;
                    let peppers = cfg
                        .subsonic_index
                        .as_ref()
                        .ok_or(Unavailable)?
                        .keyring()
                        .map_err(|_| Unavailable)?;
                    let db = env
                        .hyperdrive("SUBSONIC_MEASUREMENT_DB")
                        .map_err(|_| Unavailable)?;
                    DB_BUSY
                        .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
                        .map_err(|_| BusyOrCapacity)?;
                    acquired.set(true);
                    let pg_config: tokio_postgres::Config =
                        db.connection_string().parse().map_err(|_| Unavailable)?;
                    let tracked = socket.open(db.host(), db.port()).map_err(|_| Unavailable)?;
                    tracked.opened().await.map_err(|_| Unavailable)?;
                    let (client, connection) = pg_config
                        .connect_raw(tracked, socket.tls())
                        .await
                        .map_err(|_| Unavailable)?;
                    driver.replace(Some(Box::pin(connection)));
                    Ok(StagingApp::Postgres {
                        client,
                        peppers,
                        keks: cfg,
                        now_seconds: now_ms / 1000,
                    })
                },
                |principal| {
                    lease
                        .allows_principal(principal.user_id())
                        .then_some(CatalogueSource::<NoMirror>::Synthetic(fixture))
                },
            ),
            8_000, // Reserve the last two seconds of the request for close.
        );
        let drive = poll_fn(|cx| match driver.borrow_mut().as_mut() {
            Some(connection) => connection.as_mut().poll(cx),
            None => Poll::Pending,
        });
        // Operation is polled first: it can install the driver that the second
        // future polls on this turn. Neither future nor the client is spawned.
        match select(std::pin::pin!(operation), std::pin::pin!(drive)).await {
            Either::Left((response, _)) => response,
            Either::Right(_) => reject(Unavailable),
        }
    };
    drop(driver);
    if acquired.get() {
        let close = async { socket.close_confirmed().await.map_err(|_| Unavailable) };
        if with_deadline(close, 2_000, Unavailable).await.is_err() {
            return reject(Unavailable);
        }
        DB_BUSY.store(false, Ordering::Release);
    }
    response
}
