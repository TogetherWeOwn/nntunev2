//! Off-route Worker response channel over the pinned SDK's fixed-length Web
//! Stream. No fetch handler, source service, account operation or public miss
//! path is installed here. Writer promise resolution acknowledges sink
//! acceptance, not remote receipt; actual Worker/abort qualification is open.

use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};

use nntune_core::original_source::{SourceError, MAX_ARTICLE_BYTES};
use worker::{
    js_sys::{BigInt, Uint8Array},
    wasm_bindgen_futures::JsFuture,
    web_sys::WritableStreamDefaultWriter,
    worker_sys::FixedLengthStream,
    Headers, Response, ResponseBody,
};

use super::{OriginalPlay, OriginalWriter};

type PendingPromise = Pin<Box<JsFuture>>;

/// Build a response before pumping it, so the client may consume concurrently
/// with writes. Pass the returned writer to `OriginalPlay::pump` with a wakeable
/// request/grant control and the existing account supplier. Never await the
/// whole pump before returning the response: writes need a consuming reader.
/// The caller owns scheduling/lifetime and delivered-report persistence.
pub fn response_channel(
    play: &OriginalPlay<'_>,
) -> worker::Result<(Response, WorkerOriginalWriter)> {
    let headers = Headers::new();
    for (name, value) in &play.headers {
        headers.set(name, value)?;
    }
    let stream = if play.body_len < u32::MAX as u64 {
        FixedLengthStream::new(play.body_len as u32)
    } else {
        FixedLengthStream::new_big_int(BigInt::from(play.body_len))
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
        .with_status(play.status)
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
