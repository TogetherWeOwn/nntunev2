//! Off-route Worker response channel over the pinned SDK's fixed-length Web
//! Stream. No fetch handler, source service, account operation or public miss
//! path is installed here. Writer promise resolution acknowledges sink
//! acceptance, not remote receipt; actual Worker/abort qualification is open.

use std::{
    future::Future,
    pin::Pin,
    rc::Rc,
    task::{Context, Poll},
};

use nntune_core::original_source::{
    reader::{AsyncArticleSource, AsyncReadControl, ReadControl},
    SourceError, MAX_ARTICLE_BYTES,
};
use worker::{
    js_sys::{BigInt, Uint8Array},
    wasm_bindgen::{closure::Closure, JsCast},
    wasm_bindgen_futures::JsFuture,
    web_sys::{AbortSignal, WritableStreamDefaultWriter},
    worker_sys::FixedLengthStream,
    Date, Delay, Env, Headers, Request, Response, ResponseBody,
};

use super::{
    control::OriginalRequestControl, OriginalAuthorization, OriginalControl, OriginalPlay,
    OriginalWriter, OwnedOriginalPlay,
};
use crate::delivered::{DeliveredInterval, DeliveredSink};

/// Start only an already-prepared off-route response. The task owns its source,
/// supplier, request control and writer; the response is returned without waiting
/// for backpressure. No source lookup, account construction or public route is
/// selected here. Construct the control before resolving the private source;
/// a production supplier must share that same request authority with the response,
/// not construct an independent clock, timer or cancellation authority here.
/// Runtime lifetime limits and abort/report durability need Worker qualification.
pub fn schedule_response<T, C>(
    play: OwnedOriginalPlay,
    mut transport: T,
    mut control: C,
    env: Env,
    ctx: worker::Context,
) -> worker::Result<Response>
where
    T: AsyncArticleSource + 'static,
    C: OriginalControl + 'static,
{
    let (response, writer) = channel(play.status(), play.headers(), play.body_len())?;
    let ctx = Rc::new(ctx);
    let mut sink = WorkerDeliveredSink {
        env,
        ctx: ctx.clone(),
    };
    ctx.wait_until(async move {
        // The writer aborts on error/drop; the response settles its ACK prefix
        // through the sink. Never turn an interrupted body into a successful EOF.
        let _ = play
            .pump(&mut transport, &mut control, &mut sink, writer)
            .await;
    });
    Ok(response)
}

struct WorkerDeliveredSink {
    env: Env,
    ctx: Rc<worker::Context>,
}

impl DeliveredSink for WorkerDeliveredSink {
    fn delivered(&mut self, interval: DeliveredInterval) {
        self.ctx
            .wait_until(crate::runtime::report_delivered(self.env.clone(), interval));
    }
}

type PendingPromise = Pin<Box<JsFuture>>;
type GrantControl = OriginalRequestControl<Pin<Box<dyn Future<Output = ()>>>, fn() -> u64>;

fn now_unix() -> u64 {
    Date::now().as_millis() / 1000
}

/// Request-owned off-route control. Construct immediately after authorization,
/// before awaiting source resolution; retain through response close. The caller
/// still owns pump scheduling and the account supplier's operation timer.
pub struct WorkerOriginalControl {
    control: GrantControl,
    signal: AbortSignal,
    on_abort: Closure<dyn FnMut()>,
}

impl WorkerOriginalControl {
    pub fn new(authority: &OriginalAuthorization, request: &Request) -> worker::Result<Self> {
        let expiry_unix = authority.grant.exp_unix;
        let timer: Pin<Box<dyn Future<Output = ()>>> = Box::pin(async move {
            loop {
                // Compute on poll, not construction: a delayed first poll must
                // not extend the grant. Clamp to the SDK's signed timer range.
                let remaining = expiry_unix
                    .saturating_mul(1000)
                    .saturating_sub(Date::now().as_millis());
                if remaining == 0 {
                    break;
                }
                Delay::from(std::time::Duration::from_millis(
                    remaining.min(i32::MAX as u64),
                ))
                .await;
            }
        });
        let control = OriginalRequestControl::new(expiry_unix, timer, now_unix as fn() -> u64);
        let cancellation = control.cancellation();
        let signal = request.inner().signal();
        let on_abort = Closure::wrap(Box::new(move || cancellation.cancel()) as Box<dyn FnMut()>);
        signal
            .add_event_listener_with_callback("abort", on_abort.as_ref().unchecked_ref())
            .map_err(worker::Error::from)?;
        // An already-aborted signal emits no new event on registration.
        if signal.aborted() {
            control.cancellation().cancel();
        }
        Ok(Self {
            control,
            signal,
            on_abort,
        })
    }
}

impl ReadControl for WorkerOriginalControl {
    fn check(&mut self) -> Result<(), SourceError> {
        if self.signal.aborted() {
            self.control.cancellation().cancel();
        }
        self.control.check()
    }
}

impl AsyncReadControl for WorkerOriginalControl {
    fn poll_cancel(&mut self, cx: &mut Context<'_>) -> Poll<SourceError> {
        if let Err(error) = self.check() {
            return Poll::Ready(error);
        }
        self.control.poll_cancel(cx)
    }
}

impl OriginalControl for WorkerOriginalControl {
    fn now_unix(&self) -> u64 {
        self.control.now_unix()
    }
}

impl Drop for WorkerOriginalControl {
    fn drop(&mut self) {
        let _ = self
            .signal
            .remove_event_listener_with_callback("abort", self.on_abort.as_ref().unchecked_ref());
    }
}

/// Build a response before pumping it, so the client may consume concurrently
/// with writes. Pass the returned writer to `OriginalPlay::pump` with a wakeable
/// request/grant control and the existing account supplier. Never await the
/// whole pump before returning the response: writes need a consuming reader.
/// The caller owns scheduling/lifetime and delivered-report persistence.
pub fn response_channel(
    play: &OriginalPlay<'_>,
) -> worker::Result<(Response, WorkerOriginalWriter)> {
    channel(play.status, &play.headers, play.body_len)
}

fn channel(
    status: u16,
    values: &[(String, String)],
    body_len: u64,
) -> worker::Result<(Response, WorkerOriginalWriter)> {
    let headers = Headers::new();
    for (name, value) in values {
        headers.set(name, value)?;
    }
    let stream = if body_len < u32::MAX as u64 {
        FixedLengthStream::new(body_len as u32)
    } else {
        FixedLengthStream::new_big_int(BigInt::from(body_len))
    }
    .map_err(worker::Error::from)?;
    let raw = stream
        .writable()
        .get_writer()
        .map_err(worker::Error::from)?;
    let writer = WorkerOriginalWriter {
        closed: Box::pin(JsFuture::from(raw.closed())),
        raw,
        writing: None,
        closing: None,
        stopped: false,
    };
    let response = Response::from_body(ResponseBody::Stream(stream.readable()))?
        .with_status(status)
        .with_headers(headers);
    Ok((response, writer))
}

/// One bounded JS chunk/write promise at a time. The Web Stream's default
/// backpressure prevents another write from being queued until the current
/// write resolves; the pump does not pull another article in that interval.
pub struct WorkerOriginalWriter {
    raw: WritableStreamDefaultWriter,
    closed: PendingPromise,
    writing: Option<(usize, PendingPromise)>,
    closing: Option<PendingPromise>,
    stopped: bool,
}

impl OriginalWriter for WorkerOriginalWriter {
    fn poll_cancel(&mut self, cx: &mut Context<'_>) -> Poll<SourceError> {
        if self.stopped || self.closed.as_mut().poll(cx).is_ready() {
            Poll::Ready(SourceError::Cancelled)
        } else {
            Poll::Pending
        }
    }

    fn poll_write(
        &mut self,
        cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<Result<usize, SourceError>> {
        if self.stopped || self.closing.is_some() {
            return Poll::Ready(Err(SourceError::Cancelled));
        }
        if bytes.is_empty() || bytes.len() > MAX_ARTICLE_BYTES {
            return Poll::Ready(Err(SourceError::Bounds));
        }
        let (length, promise) = self.writing.get_or_insert_with(|| {
            let chunk = Uint8Array::from(bytes);
            (
                bytes.len(),
                Box::pin(JsFuture::from(self.raw.write_with_chunk(&chunk))),
            )
        });
        if *length != bytes.len() {
            return Poll::Ready(Err(SourceError::Bounds));
        }
        match promise.as_mut().poll(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(result) => {
                let length = *length;
                self.writing = None;
                Poll::Ready(result.map(|_| length).map_err(|_| SourceError::Transport))
            }
        }
    }

    fn poll_close(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), SourceError>> {
        if self.stopped || self.writing.is_some() {
            return Poll::Ready(Err(SourceError::Cancelled));
        }
        let closing = self
            .closing
            .get_or_insert_with(|| Box::pin(JsFuture::from(self.raw.close())));
        match closing.as_mut().poll(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Err(_)) => Poll::Ready(Err(SourceError::Transport)),
            Poll::Ready(Ok(_)) => {
                self.stopped = true;
                self.raw.release_lock();
                Poll::Ready(Ok(()))
            }
        }
    }

    fn abort(&mut self) {
        if self.stopped {
            return;
        }
        self.stopped = true;
        // Attach rejection handling, then request abort without awaiting it.
        // No source/account I/O or hidden drain is started during Drop.
        drop(JsFuture::from(self.raw.abort()));
        self.raw.release_lock();
        self.writing = None;
        self.closing = None;
    }
}

impl Drop for WorkerOriginalWriter {
    fn drop(&mut self) {
        self.abort();
    }
}
