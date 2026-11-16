//! Request-owned Worker socket preparation. This is not connected to routing.

use std::{cell::RefCell, future::Ready, io, rc::Rc};
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
