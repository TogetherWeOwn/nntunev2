//! Request-owned Worker socket preparation. This is not connected to routing.

use std::{cell::RefCell, io, rc::Rc};
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
