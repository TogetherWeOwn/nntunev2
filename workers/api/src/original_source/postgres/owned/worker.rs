//! Off-route Hyperdrive qualification adapter. Authentication and a signed
//! whole-call timer must enclose connect and all queries via service::read_timed.

use std::{
    cell::{Cell, RefCell},
    future::Future,
    io,
    pin::Pin,
    rc::Rc,
};

use tokio_postgres::tls::TlsConnect;
use worker::{
    js_sys::{Object, Reflect},
    wasm_bindgen::JsValue,
    worker_sys, Env, Socket,
};

use super::{super::ClientRepository, DrivenRepository, SourceServiceError};
use crate::original_source::service::RepositoryFactory;

struct RawSocket {
    current: RefCell<worker_sys::Socket>,
    closing: Cell<bool>,
}

impl RawSocket {
    fn close(&self) {
        if !self.closing.replace(true) {
            // Synchronous forced-close initiation, not a physical-close ACK.
            let _ = self.current.borrow().close();
        }
    }
}

impl Drop for RawSocket {
    fn drop(&mut self) {
        self.close();
    }
}

#[derive(Clone)]
struct RetainedTls(Rc<RawSocket>);

impl TlsConnect<Socket> for RetainedTls {
    type Stream = Socket;
    type Error = io::Error;
    type Future = std::future::Ready<Result<Socket, io::Error>>;

    fn connect(self, _socket: Socket) -> Self::Future {
        let result = (|| {
            let tls = self
                .0
                .current
                .borrow()
                .start_tls()
                .map_err(|_| io::Error::other("source database TLS failed"))?;
            // STARTTLS replaces the socket. Retain its close authority before
            // handing it to the PostgreSQL handshake, not the invalid old one.
            *self.0.current.borrow_mut() = tls.clone();
            Ok(Socket::from(tls))
        })();
        std::future::ready(result)
    }
}

type Driver = Pin<Box<dyn Future<Output = Result<(), SourceServiceError>>>>;
type Close = Box<dyn Fn()>;

pub struct WorkerRepositoryFactory {
    env: Env,
}

impl WorkerRepositoryFactory {
    /// Inert: no binding access, socket or driver until authenticated connect.
    pub fn new(env: Env) -> Self {
        Self { env }
    }
}

impl RepositoryFactory for WorkerRepositoryFactory {
    type Repository = DrivenRepository<ClientRepository, Driver, Close>;

    async fn connect(&self) -> Result<Self::Repository, SourceServiceError> {
        let failure = || SourceServiceError::Store;
        if !crate::worker_entry::hyperdrive_caching_disabled_attested(
            self.env
                .var("HYPERDRIVE_CACHING_DISABLED")
                .ok()
                .map(|value| value.to_string()),
        ) {
            return Err(failure());
        }
        let binding = self.env.hyperdrive("HYPERDRIVE").map_err(|_| failure())?;
        let mut config: tokio_postgres::Config =
            binding.connection_string().parse().map_err(|_| failure())?;
        // No cleartext fallback if the proxy rejects PostgreSQL STARTTLS.
        config.ssl_mode(tokio_postgres::config::SslMode::Require);
        let address = Object::new();
        Reflect::set(&address, &"hostname".into(), &binding.host().into())
            .map_err(|_| failure())?;
        Reflect::set(&address, &"port".into(), &JsValue::from(binding.port()))
            .map_err(|_| failure())?;
        let options = Object::new();
        Reflect::set(&options, &"secureTransport".into(), &"starttls".into())
            .map_err(|_| failure())?;
        Reflect::set(&options, &"allowHalfOpen".into(), &JsValue::FALSE).map_err(|_| failure())?;
        let raw = worker_sys::connect(address.into(), options.into()).map_err(|_| failure())?;
        // Lives across opening, TLS and PostgreSQL authentication, even when
        // the pending connect future is dropped. No spawn or detached cleanup.
        let retained = Rc::new(RawSocket {
            current: RefCell::new(raw.clone()),
            closing: Cell::new(false),
        });
        let socket = Socket::from(raw);
        socket.opened().await.map_err(|_| failure())?;
        let (client, driver) = config
            .connect_raw(socket, RetainedTls(retained.clone()))
            .await
            .map_err(|_| failure())?;
        let driver: Driver = Box::pin(async move { driver.await.map_err(|_| failure()) });
        let close: Close = Box::new(move || {
            retained.close();
        });
        Ok(DrivenRepository::new(
            ClientRepository::new(client),
            driver,
            close,
        ))
    }
}
