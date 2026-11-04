//! Off-route catalogue/account supplier. Inherent methods only until the shared
//! async library lands; no second reader, HTTP writer or delivered-byte meter.

use std::{future::Future, task::Poll};

use futures_util::{future::Shared, FutureExt};
use nntune_core::original_source::{OriginalSource, SourceError};

use super::{control::RequestControl, control::Signal, OriginalCatalogue};
use crate::{
    account_dispatch::OperationReply,
    account_operation::{Operation, Output},
    account_pool::{WorkClass, SESSION_TTL_MS},
    account_route::{ChannelKeys, RegistryError, RegistrySource},
    account_selection::{self, AccountSource},
};

/// Internal telemetry only, never delivery, entitlement or public metadata.
/// No Debug: neither encoded articles nor source/account identities are logged.
pub struct ArticleSupplier<'a, R, A> {
    source: OriginalSource,
    opened: Vec<bool>,
    serving_accounts: Vec<String>,
    control: RequestControl<'a>,
    registry: &'a R,
    accounts: &'a A,
    keys: ChannelKeys<'a>,
    operation_timer: &'a dyn Fn(u64) -> Signal<'a>,
}

impl<'a, R: RegistrySource, A: AccountSource> ArticleSupplier<'a, R, A> {
    /// `control` must be the already verified request authority; the response
    /// consumer uses a clone of that SAME authority. Lookup is lazy, raced
    /// against request cancellation/expiry, and never repeated by this supplier.
    /// `operation_timer` receives exact remaining milliseconds for each open and
    /// must wake by that deadline. It performs no I/O during preparation.
    /// Runtime must check qualification before constructing any of these inputs.
    pub async fn prepare(
        catalogue: &impl OriginalCatalogue,
        registry: &'a R,
        accounts: &'a A,
        keys: ChannelKeys<'a>,
        control: RequestControl<'a>,
        operation_timer: &'a dyn Fn(u64) -> Signal<'a>,
    ) -> Result<Self, SourceError> {
        let source = control
            .run(async {
                if !keys.account.is_some_and(|key| key.len() >= 32)
                    || !keys.resolver.is_some_and(|key| key.len() >= 32)
                {
                    return Err(SourceError::Transport);
                }
                let source = catalogue
                    .source(control.track_id())
                    .await
                    .map_err(|_| SourceError::Transport)?
                    .ok_or(SourceError::Transport)?;
                if source.track_id() != control.track_id() {
                    return Err(SourceError::Binding);
                }
                Ok(source)
            })
            .await?;
        Ok(Self {
            opened: vec![false; source.article_count()],
            serving_accounts: Vec::new(),
            source,
            control,
            registry,
            accounts,
            keys,
            operation_timer,
        })
    }

    pub fn source(&self) -> &OriginalSource {
        &self.source
    }

    /// One actual account per successful encoded article, including proof reads.
    /// This is not an instruction to publish, cache, bill or record delivery.
    pub fn serving_accounts(&self) -> &[String] {
        &self.serving_accounts
    }

    /// Bind BOTH the zero-based trusted source index and exact reference. Opens
    /// may be nonsequential (the reader proves the final article first), but no
    /// article can be attempted twice, even after uncertainty, failure or drop.
    /// Account transports must bound bytes while reading; selection additionally
    /// rejects an oversized or wrong-kind reply before returning it here.
    pub async fn open(&mut self, index: usize, reference: &str) -> Result<Vec<u8>, SourceError> {
        let control = self.control.clone();
        let (provider_id, bytes) = control
            .run(async {
                let article = self.source.article(index)?;
                if article.reference() != reference || self.opened[index] {
                    return Err(SourceError::Binding);
                }
                let operation = Operation::Body(article.reference().to_owned());
                if !operation.valid() {
                    return Err(SourceError::Binding);
                }
                self.opened[index] = true;
                let deadline_ms = control
                    .observed_ms()
                    .saturating_add(SESSION_TTL_MS)
                    .min(control.deadline_ms());
                let remaining_ms = deadline_ms - control.observed_ms();
                // One timer across registry, hints and BODY, never per stage.
                let scope = OperationScope {
                    control: control.clone(),
                    deadline_ms,
                    timer: (self.operation_timer)(remaining_ms).shared(),
                };
                let guarded = Guarded {
                    scope: &scope,
                    registry: self.registry,
                    accounts: self.accounts,
                };
                let served = scope
                    .run(account_selection::run(
                        &guarded,
                        &guarded,
                        ChannelKeys {
                            account: self.keys.account,
                            resolver: self.keys.resolver,
                        },
                        WorkClass::Stream,
                        operation,
                        || control.observed_ms(),
                        std::future::pending::<()>(),
                    ))
                    .await?
                    .map_err(|_| SourceError::Transport)?;
                let Output::Body(bytes) = served.output else {
                    return Err(SourceError::Transport);
                };
                if bytes.is_empty() {
                    return Err(SourceError::Transport);
                }
                Ok((served.provider_id, bytes))
            })
            .await?;
        self.serving_accounts.push(provider_id);
        Ok(bytes)
    }
}

struct OperationScope<'a> {
    control: RequestControl<'a>,
    deadline_ms: u64,
    timer: Shared<Signal<'a>>,
}

impl OperationScope<'_> {
    fn check(&self) -> Result<(), SourceError> {
        self.control.check_live()?;
        if self.control.observed_ms() >= self.deadline_ms {
            self.control.stop();
            return Err(SourceError::Cancelled);
        }
        Ok(())
    }

    // Control failures revoke all request clones; ordinary candidate failures
    // remain values so selection can ignore an unavailable hint without retrying
    // an operation. Each wait has its own shared-timer registration.
    async fn run<T>(&self, work: impl Future<Output = T>) -> Result<T, SourceError> {
        let mut timer = self.timer.clone();
        let mut work = std::pin::pin!(work);
        self.control
            .run(std::future::poll_fn(|cx| {
                self.check()?;
                if timer.poll_unpin(cx).is_ready() {
                    return Poll::Ready(Err(SourceError::Cancelled));
                }
                self.check()?;
                let result = work.as_mut().poll(cx);
                self.check()?;
                if timer.poll_unpin(cx).is_ready() {
                    return Poll::Ready(Err(SourceError::Cancelled));
                }
                self.check()?;
                result.map(Ok)
            }))
            .await
    }
}

// Checks at every awaited I/O boundary prevent a synchronously-ready registry
// or hint from consuming authority and dispatching BODY in the same outer poll.
struct Guarded<'s, 'a, R, A> {
    scope: &'s OperationScope<'a>,
    registry: &'a R,
    accounts: &'a A,
}

impl<R: RegistrySource, A> RegistrySource for Guarded<'_, '_, R, A> {
    async fn fetch(&self, authorization: &str) -> Result<Vec<u8>, RegistryError> {
        self.scope
            .run(self.registry.fetch(authorization))
            .await
            .map_err(|_| RegistryError)?
    }
}

impl<R, A: AccountSource> AccountSource for Guarded<'_, '_, R, A> {
    async fn candidate(
        &self,
        account_id: &str,
        authorization: &str,
        body: &[u8],
    ) -> Result<Vec<u8>, RegistryError> {
        self.scope
            .run(self.accounts.candidate(account_id, authorization, body))
            .await
            .map_err(|_| RegistryError)?
    }

    async fn execute(&self, account_id: &str, authorization: &str, body: &[u8]) -> OperationReply {
        self.scope
            .run(self.accounts.execute(account_id, authorization, body))
            .await
            .unwrap_or(OperationReply::Failed(
                crate::account_operation::OperationError::Unavailable,
            ))
    }
}

#[cfg(test)]
mod tests;
