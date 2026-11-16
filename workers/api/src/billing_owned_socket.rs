//! Request-owned Worker socket preparation. This is not connected to routing.

use std::{
    cell::RefCell,
    future::{poll_fn, Future, Ready},
    io,
    rc::Rc,
    task::Poll,
};
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

/// Owns all raw handles, including the replacement returned by STARTTLS.
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
    pub(crate) fn open(&self, host: &str, port: u16) -> Result<Socket, io::Error> {
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
        self.register(raw)
    }

    pub(crate) fn tls(&self) -> TrackedTls {
        TrackedTls(self.0.clone())
    }
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

/// Poll both deadlines before the driver and operation, without spawning either.
/// The caller must keep all client handles inside `operation` and arm the
/// total and current phase timers before their respective work begins. Dropping
/// this future closes the request's sockets even when neither future completes.
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

// Exercises the real WASM future and its owned guard, not socket shutdown.
#[cfg(feature = "billing-owned-wasm-test")]
pub(crate) fn test_drive_owned() -> Result<(), &'static str> {
    use std::{cell::Cell, future, task::Context};

    let waker = futures_util::task::noop_waker_ref();
    let mut cx = Context::from_waker(waker);
    let guard = SocketGuard::default();
    let registry = guard.0.clone();
    let polled = Cell::new(false);
    let operation = poll_fn(|_| {
        polled.set(true);
        Poll::Ready(Ok::<(), ()>(()))
    });
    let mut future = Box::pin(drive_owned(
        guard,
        future::ready(()),
        future::ready(()),
        future::ready(Ok::<(), ()>(())),
        operation,
    ));
    if !matches!(
        future.as_mut().poll(&mut cx),
        Poll::Ready(Err(DriveError::Expired))
    ) || polled.get()
        || !registry.borrow().closed
    {
        return Err("total deadline did not win");
    }

    let guard = SocketGuard::default();
    let registry = guard.0.clone();
    let mut future = Box::pin(drive_owned(
        guard,
        future::pending(),
        future::ready(()),
        future::ready(Ok::<(), ()>(())),
        future::ready(Ok::<(), ()>(())),
    ));
    if !matches!(
        future.as_mut().poll(&mut cx),
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
    let mut future = Box::pin(drive_owned(
        guard,
        future::pending(),
        future::pending(),
        driver,
        operation,
    ));
    if !future.as_mut().poll(&mut cx).is_pending() {
        return Err("pending operation completed");
    }
    if !driver_polled.get() || !operation_polled.get() {
        return Err("driver and operation were not co-polled in order");
    }
    drop(future);
    if !registry.borrow().closed {
        return Err("dropped future did not close its guard");
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

pub(crate) struct TrackedTls(Rc<RefCell<Registry>>);

impl TlsConnect<Socket> for TrackedTls {
    type Stream = Socket;
    type Error = io::Error;
    type Future = Ready<Result<Socket, io::Error>>;

    fn connect(self, socket: Socket) -> Self::Future {
        let result = (|| {
            let mut registry = self.0.borrow_mut();
            if registry.closed || registry.sockets.len() != 1 {
                return Err(io::Error::other("socket owner unavailable"));
            }
            let raw = &registry.sockets[0];
            let upgraded = raw
                .start_tls()
                .map_err(|_| io::Error::other("TLS upgrade failed"))?;
            registry.sockets.push(upgraded.clone());
            drop(socket);
            Ok(Socket::from(upgraded))
        })();
        std::future::ready(result)
    }
}
