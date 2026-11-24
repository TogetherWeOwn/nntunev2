//! Protected staging evidence for one authorized synthetic media request.

use std::{cell::RefCell, pin::pin, time::Duration};

use futures_util::future::{select, Either};
use worker::{
    js_sys::{Date, Object, Promise},
    wasm_bindgen_futures::JsFuture,
    web_sys::AbortSignal,
    Delay, Env, Request,
};

use super::s3_transport::hex;
use crate::{grant::sha256, trace};

pub(super) struct ReadObservation {
    trace: String,
    cancellation: RefCell<Option<(Promise, AbortSignal)>>,
}

impl ReadObservation {
    /// Configuration uses a noncredential run trace and the digest of a
    /// dedicated synthetic track ID. The caller supplies a verified v4 track.
    pub(super) fn from_env(env: &Env, req: &Request, track: &str) -> Option<Self> {
        if env.var("ENVIRONMENT").ok()?.to_string() != "staging" {
            return None;
        }
        let selector = env
            .var("PRIVATE_PLAYBACK_READ_OBSERVATION")
            .ok()?
            .to_string();
        let (trace, track_digest) = selector.split_once(':')?;
        let inbound = req.headers().get("x-trace-id").ok()??;
        if !trace::is_valid_trace_id(trace)
            || trace != inbound
            || track_digest != hex(&sha256(track.as_bytes()))
        {
            return None;
        }
        Some(Self {
            trace: inbound,
            cancellation: RefCell::new(None),
        })
    }

    pub(super) fn started(&self) {
        self.log("private_storage_read_started");
    }

    pub(super) fn completed(&self) {
        self.log("private_storage_read_completed");
    }

    pub(super) fn dropping_pending_read(&self, read: Promise, signal: AbortSignal) {
        self.cancellation.replace(Some((read, signal)));
    }

    pub(super) async fn confirm_abort(&self) {
        let Some((read, signal)) = self.cancellation.take() else {
            return;
        };
        let read = pin!(JsFuture::from(read));
        let timeout = pin!(Delay::from(Duration::from_secs(1)));
        // The pending Fetch read must reject with this controller's reason.
        // A fulfilled read, independent error or expired witness is inconclusive.
        if let Either::Left((Err(error), _)) = select(read, timeout).await {
            if signal.aborted() && Object::is(&error, &signal.reason()) {
                self.log("private_storage_read_aborted");
            }
        }
    }

    fn log(&self, event: &str) {
        if let Some(line) = trace::render_log_line(
            &String::from(Date::new_0().to_iso_string()),
            "info",
            &self.trace,
            event,
            &trace::LogFields::new(),
        ) {
            worker::console_log!("{}", line);
        }
    }
}
