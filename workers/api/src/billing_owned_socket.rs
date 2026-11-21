//! Request-owned Worker socket preparation. This is not connected to routing.

use std::{
    cell::RefCell,
    future::{poll_fn, Future},
    io,
    pin::Pin,
    rc::Rc,
    task::{Context, Poll},
};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio_postgres::tls::TlsConnect;
use wasm_bindgen::prelude::*;
use worker::{js_sys, worker_sys, Socket};

#[wasm_bindgen(module = "/src/billing_socket_close.js")]
extern "C" {
    #[wasm_bindgen(js_name = observeSocketClose)]
    fn observe_socket_close(socket: &worker_sys::Socket);
}

#[derive(Default)]
struct Registry {
    sockets: Vec<worker_sys::Socket>,
    closed: bool,
}

/// Owns the raw socket handle until the request ends.
#[derive(Default)]
pub(crate) struct SocketGuard(Rc<RefCell<Registry>>);

impl SocketGuard {
    fn register(&self, socket: worker_sys::Socket) -> Result<Socket, io::Error> {
        let mut registry = self.0.borrow_mut();
        if registry.closed {
            observe_socket_close(&socket);
            return Err(io::Error::other("socket owner closed"));
        }
        registry.sockets.push(socket.clone());
        Ok(Socket::from(socket))
    }

    /// Open the same host and STARTTLS transport selected by Hyperdrive.
    pub(crate) fn open(&self, host: &str, port: u16) -> Result<TrackedSocket, io::Error> {
        {
            let registry = self.0.borrow();
            if registry.closed || !registry.sockets.is_empty() {
                return Err(io::Error::other("socket owner already used"));
            }
        }
        let address = js_sys::Object::new();
        let options = js_sys::Object::new();
        let set = |object: &js_sys::Object, key: &str, value: &JsValue| {
            js_sys::Reflect::set(object, &JsValue::from_str(key), value)
                .map_err(|_| io::Error::other("socket configuration failed"))
                .and_then(|ok| {
                    ok.then_some(())
                        .ok_or_else(|| io::Error::other("socket configuration failed"))
                })
        };
        set(&address, "hostname", &JsValue::from_str(host))?;
        set(&address, "port", &JsValue::from_f64(f64::from(port)))?;
        set(&options, "allowHalfOpen", &JsValue::FALSE)?;
        set(&options, "secureTransport", &JsValue::from_str("starttls"))?;
        let raw = worker_sys::connect(address.into(), options.into())
            .map_err(|_| io::Error::other("socket connect failed"))?;
        self.track(raw)
    }

    fn track(&self, raw: worker_sys::Socket) -> Result<TrackedSocket, io::Error> {
        let socket = self.register(raw.clone())?;
        Ok(TrackedSocket {
            socket,
            raw,
            owner: self.0.clone(),
        })
    }

    #[cfg(feature = "billing-owned-wasm-test")]
    pub(crate) fn track_fixture(
        &self,
        raw: worker_sys::Socket,
    ) -> Result<TrackedSocket, io::Error> {
        self.track(raw)
    }

    pub(crate) fn tls(&self) -> TrackedTls {
        TrackedTls(self.0.clone())
    }

    /// Wait for the active socket's close acknowledgement. STARTTLS replaces
    /// the original handle; it does not open a second physical connection.
    /// The caller must stop socket work first and bound this wait. Acknowledgement
    /// does not establish backend teardown.
    #[cfg_attr(not(feature = "billing-owned-wasm-test"), allow(dead_code))]
    pub(crate) async fn close_confirmed(&self) -> Result<(), JsValue> {
        let active = self.0.borrow().sockets.last().cloned();
        if let Some(socket) = active {
            worker::wasm_bindgen_futures::JsFuture::from(socket.close()?).await?;
            worker::wasm_bindgen_futures::JsFuture::from(socket.closed()?).await?;
        }
        let mut registry = self.0.borrow_mut();
        registry.closed = true;
        registry.sockets.clear();
        Ok(())
    }
}

/// The raw handle and its I/O wrapper are created together and cannot be mixed.
pub(crate) struct TrackedSocket {
    socket: Socket,
    raw: worker_sys::Socket,
    owner: Rc<RefCell<Registry>>,
}

impl TrackedSocket {
    pub(crate) async fn opened(&self) -> Result<(), io::Error> {
        self.socket
            .opened()
            .await
            .map(|_| ())
            .map_err(|_| io::Error::other("socket open failed"))
    }
}

impl AsyncRead for TrackedSocket {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.socket).poll_read(cx, buf)
    }
}

impl AsyncWrite for TrackedSocket {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.socket).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.socket).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.socket).poll_shutdown(cx)
    }
}

pub(crate) struct TrackedTls(Rc<RefCell<Registry>>);

impl TlsConnect<TrackedSocket> for TrackedTls {
    type Stream = Socket;
    type Error = io::Error;
    type Future = std::future::Ready<Result<Socket, io::Error>>;

    fn connect(self, socket: TrackedSocket) -> Self::Future {
        let result = (|| {
            if !Rc::ptr_eq(&self.0, &socket.owner) {
                return Err(io::Error::other("socket owner mismatch"));
            }
            let mut registry = self.0.borrow_mut();
            if registry.closed || registry.sockets.len() != 1 {
                return Err(io::Error::other("socket owner unavailable"));
            }
            let upgraded = socket
                .raw
                .start_tls()
                .map_err(|_| io::Error::other("TLS upgrade failed"))?;
            registry.sockets.push(upgraded.clone());
            Ok(Socket::from(upgraded))
        })();
        std::future::ready(result)
    }
}

#[cfg(feature = "billing-owned-wasm-test")]
#[wasm_bindgen(module = "/src/billing_socket_test.js")]
extern "C" {
    #[wasm_bindgen(js_name = socketFixture)]
    fn socket_fixture(acknowledged: bool) -> JsValue;
    #[wasm_bindgen(js_name = stalledStartupFixture)]
    pub(crate) fn stalled_startup_fixture(stall_open: bool) -> js_sys::Array;
}

// Synthetic JS handles verify registry ownership, not Worker network or TLS behavior.
#[cfg(feature = "billing-owned-wasm-test")]
pub(crate) async fn test_tracked_socket() -> Result<(), &'static str> {
    fn property(value: &JsValue, name: &str) -> JsValue {
        js_sys::Reflect::get(value, &JsValue::from_str(name)).unwrap()
    }
    fn count(fixture: &JsValue, name: &str) -> f64 {
        property(&property(fixture, "calls"), name)
            .as_f64()
            .unwrap()
    }
    fn tracked(guard: &SocketGuard, fixture: &JsValue) -> TrackedSocket {
        let raw: worker_sys::Socket = property(fixture, "original").unchecked_into();
        let socket = guard.register(raw.clone()).unwrap();
        TrackedSocket {
            socket,
            raw,
            owner: guard.0.clone(),
        }
    }

    let fixture = socket_fixture(false);
    let owner = SocketGuard::default();
    let other = SocketGuard::default();
    if other.tls().connect(tracked(&owner, &fixture)).await.is_ok()
        || count(&fixture, "upgrades") != 0.0
    {
        return Err("foreign guard upgraded socket");
    }
    drop(owner);
    if count(&fixture, "originalCloses") != 1.0 {
        return Err("foreign socket was not closed by its owner");
    }

    let fixture = socket_fixture(false);
    let owner = SocketGuard::default();
    let registry = owner.0.clone();
    let upgraded = owner
        .tls()
        .connect(tracked(&owner, &fixture))
        .await
        .map_err(|_| "tracked upgrade failed")?;
    if count(&fixture, "upgrades") != 1.0 || registry.borrow().sockets.len() != 2 {
        return Err("replacement was not registered before return");
    }
    drop(upgraded);
    drop(owner);
    if !registry.borrow().closed
        || !registry.borrow().sockets.is_empty()
        || count(&fixture, "originalCloses") != 1.0
        || count(&fixture, "replacementCloses") != 1.0
        || count(&fixture, "observedRejections") != 2.0
    {
        return Err("guard did not close both registered handles");
    }
    let fixture = socket_fixture(false);
    let owner = SocketGuard::default();
    let socket = tracked(&owner, &fixture);
    let task = drive_owned(
        owner,
        std::future::pending(),
        std::future::pending(),
        std::future::pending::<Result<(), ()>>(),
        async move {
            let _socket = socket;
            std::future::pending::<Result<(), ()>>().await
        },
    );
    drop(task);
    if count(&fixture, "originalCloses") != 1.0 || count(&fixture, "observedRejections") != 1.0 {
        return Err("dropped future did not close its tracked socket");
    }

    // Exercise a real Worker timer cancelling an owned future. The handle is
    // still synthetic; this does not establish network teardown or TLS trust.
    let fixture = socket_fixture(false);
    let owner = SocketGuard::default();
    let socket = tracked(&owner, &fixture);
    let result = drive_owned(
        owner,
        worker::Delay::from(std::time::Duration::from_millis(20)),
        std::future::pending(),
        std::future::pending::<Result<(), ()>>(),
        async move {
            let _socket = socket;
            std::future::pending::<Result<(), ()>>().await
        },
    )
    .await;
    if !matches!(result, Err(DriveError::Expired))
        || count(&fixture, "originalCloses") != 1.0
        || count(&fixture, "observedRejections") != 1.0
    {
        return Err("Worker timer did not close the owned synthetic socket");
    }

    let fixture = socket_fixture(false);
    let owner = SocketGuard::default();
    owner.0.borrow_mut().closed = true;
    let raw: worker_sys::Socket = property(&fixture, "original").unchecked_into();
    if owner.register(raw).is_ok() || count(&fixture, "originalCloses") != 1.0 {
        return Err("closed guard did not reject and close new handle");
    }
    drop(owner);
    if count(&fixture, "originalCloses") != 1.0 {
        return Err("rejected handle was closed twice");
    }

    let fixture = socket_fixture(true);
    let owner = SocketGuard::default();
    let upgraded = owner
        .tls()
        .connect(tracked(&owner, &fixture))
        .await
        .map_err(|_| "tracked upgrade failed")?;
    drop(upgraded);
    owner
        .close_confirmed()
        .await
        .map_err(|_| "close not confirmed")?;
    if count(&fixture, "closedAcknowledgements") != 1.0 {
        return Err("active socket close did not await acknowledgement");
    }
    drop(owner);
    if count(&fixture, "replacementCloses") != 1.0 || count(&fixture, "originalCloses") != 0.0 {
        return Err("confirmed active socket close repeated on drop");
    }
    Ok(())
}

// Observes only an empty registry; no socket is opened or closed here.
#[cfg(feature = "billing-owned-wasm-test")]
pub(crate) fn test_empty_guard_drop() -> bool {
    let guard = SocketGuard::default();
    let registry = guard.0.clone();
    let initially_empty = {
        let state = registry.borrow();
        !state.closed && state.sockets.is_empty()
    };
    drop(guard);
    let state = registry.borrow();
    initially_empty && state.closed && state.sockets.is_empty()
}

impl Drop for SocketGuard {
    fn drop(&mut self) {
        let mut registry = self.0.borrow_mut();
        registry.closed = true;
        for socket in registry.sockets.drain(..) {
            observe_socket_close(&socket);
        }
    }
}

/// Poll deadlines before the driver and operation without spawning either.
/// The caller must arm both timers before their work and keep client handles
/// inside `operation`. Dropping this future also drops the socket guard.
pub(crate) async fn drive_owned<T, E>(
    guard: SocketGuard,
    deadline: impl Future<Output = ()>,
    phase_deadline: impl Future<Output = ()>,
    driver: impl Future<Output = Result<(), E>>,
    operation: impl Future<Output = Result<T, E>>,
) -> Result<T, DriveError<E>> {
    let result = {
        let mut deadline = std::pin::pin!(deadline);
        let mut phase_deadline = std::pin::pin!(phase_deadline);
        let mut driver = std::pin::pin!(driver);
        let mut operation = std::pin::pin!(operation);
        poll_fn(|cx| {
            if deadline.as_mut().poll(cx).is_ready() {
                return Poll::Ready(Err(DriveError::Expired));
            }
            if phase_deadline.as_mut().poll(cx).is_ready() {
                return Poll::Ready(Err(DriveError::PhaseExpired));
            }
            if let Poll::Ready(result) = driver.as_mut().poll(cx) {
                return Poll::Ready(Err(DriveError::Driver(result.err())));
            }
            operation
                .as_mut()
                .poll(cx)
                .map(|result| result.map_err(DriveError::Operation))
        })
        .await
    };
    drop(guard);
    result
}

pub(crate) enum DriveError<E> {
    Expired,
    PhaseExpired,
    Driver(Option<E>),
    Operation(E),
}

// These observe an empty registry, not actual socket shutdown.
#[cfg(feature = "billing-owned-wasm-test")]
pub(crate) fn test_drive_owned() -> Result<(), &'static str> {
    use std::{cell::Cell, future, task::Context};

    let mut cx = Context::from_waker(futures_util::task::noop_waker_ref());
    let guard = SocketGuard::default();
    let registry = guard.0.clone();
    let polled = Cell::new(false);
    let operation = poll_fn(|_| {
        polled.set(true);
        Poll::Ready(Ok::<(), ()>(()))
    });
    let mut task = Box::pin(drive_owned(
        guard,
        future::ready(()),
        future::ready(()),
        future::ready(Ok::<(), ()>(())),
        operation,
    ));
    if !matches!(
        task.as_mut().poll(&mut cx),
        Poll::Ready(Err(DriveError::Expired))
    ) || polled.get()
        || !registry.borrow().closed
    {
        return Err("total deadline did not win");
    }

    let guard = SocketGuard::default();
    let registry = guard.0.clone();
    let mut task = Box::pin(drive_owned(
        guard,
        future::pending(),
        future::ready(()),
        future::ready(Ok::<(), ()>(())),
        future::ready(Ok::<(), ()>(())),
    ));
    if !matches!(
        task.as_mut().poll(&mut cx),
        Poll::Ready(Err(DriveError::PhaseExpired))
    ) || !registry.borrow().closed
    {
        return Err("phase deadline did not win");
    }

    let guard = SocketGuard::default();
    let registry = guard.0.clone();
    let driver_polled = Cell::new(false);
    let operation_polled = Cell::new(false);
    let driver = poll_fn(|_| {
        driver_polled.set(true);
        Poll::Pending::<Result<(), ()>>
    });
    let operation = poll_fn(|_| {
        operation_polled.set(driver_polled.get());
        Poll::Pending::<Result<(), ()>>
    });
    let mut task = Box::pin(drive_owned(
        guard,
        future::pending(),
        future::pending(),
        driver,
        operation,
    ));
    if !task.as_mut().poll(&mut cx).is_pending() || !driver_polled.get() || !operation_polled.get()
    {
        return Err("driver and operation were not co-polled in order");
    }
    drop(task);
    if !registry.borrow().closed {
        return Err("dropped future did not close its empty registry");
    }
    Ok(())
}

#[cfg(feature = "billing-owned-wasm-test")]
pub(crate) async fn test_worker_timer() -> Result<(), &'static str> {
    let result = drive_owned(
        SocketGuard::default(),
        worker::Delay::from(std::time::Duration::from_millis(20)),
        std::future::pending(),
        std::future::pending::<Result<(), ()>>(),
        std::future::pending::<Result<(), ()>>(),
    )
    .await;
    if matches!(result, Err(DriveError::Expired)) {
        Ok(())
    } else {
        Err("Worker timer did not expire")
    }
}
