//! Pool-owned response handoff. The reviewed response library remains the only
//! reader, writer, ACK-prefix meter and scheduler; this module retains its inputs.

use super::*;
use crate::{
    delivered::DeliveredSink,
    original_play::{OriginalAuthorization, OriginalWriter, OwnedOriginalPlay},
    play_route::PlayRouteRefusal,
};

/// A checked response plus all request-owned pool dependencies. No article I/O
/// occurs until its pump is polled. This does not qualify or enable ingress.
pub struct OwnedAccountResponse<R: 'static, A: 'static> {
    play: OwnedOriginalPlay,
    supplier: ArticleSupplier<'static, R, A>,
    control: RequestControl<'static>,
}

impl<R: RegistrySource + 'static, A: AccountSource + 'static> ArticleSupplier<'static, R, A> {
    /// Consume the retained supplier after lookup. `authorization` must come
    /// from the same verified request used to construct its control. The plan
    /// shares the exact source allocation and a clone of that SAME authority;
    /// callers cannot replace either when pumping or scheduling the response.
    pub fn into_response(
        self,
        authorization: OriginalAuthorization,
        range: Option<&str>,
    ) -> Result<OwnedAccountResponse<R, A>, PlayRouteRefusal> {
        let mut control = self.control.clone();
        let play = authorization.prepare_owned(self.shared_source(), range, &mut control)?;
        Ok(OwnedAccountResponse {
            play,
            supplier: self,
            control,
        })
    }
}

impl<R: RegistrySource + 'static, A: AccountSource + 'static> OwnedAccountResponse<R, A> {
    pub fn status(&self) -> u16 {
        self.play.status()
    }

    pub fn headers(&self) -> &[(String, String)] {
        self.play.headers()
    }

    pub fn body_len(&self) -> u64 {
        self.play.body_len()
    }

    /// Construct the actual owned response future, installing the library's
    /// writer abort guard NOW, before an outer scheduler can drop it unpolled.
    /// No borrowed keys, adapters, clocks, signals or timers escape into it.
    pub fn pump_owned<S: DeliveredSink + 'static, W: OriginalWriter + 'static>(
        self,
        sink: S,
        writer: W,
    ) -> impl Future<Output = Result<(), SourceError>> + 'static {
        self.play
            .pump_owned(self.supplier, self.control, sink, writer)
    }

    /// Off-route only. Runtime qualification must precede construction; the
    /// existing Worker scheduler owns channel, task and delivered reporting.
    #[cfg(target_arch = "wasm32")]
    pub fn schedule_response(
        self,
        env: worker::Env,
        ctx: worker::Context,
    ) -> worker::Result<worker::Response> {
        crate::original_play::worker::schedule_response(
            self.play,
            self.supplier,
            self.control,
            env,
            ctx,
        )
    }
}
