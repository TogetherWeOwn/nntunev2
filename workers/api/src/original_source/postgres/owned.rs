//! Request-owned driver: no spawned task survives a source lookup. A failed or
//! cancelled query poisons the repository and initiates closure immediately.

use std::{cell::Cell, future::Future, pin::Pin, task::Poll};

use super::super::{AsyncSourceRepository, FieldEnvelope, SourceMetadata, SourceServiceError};

#[cfg(target_arch = "wasm32")]
mod worker;
#[cfg(target_arch = "wasm32")]
pub use worker::WorkerRepositoryFactory;

struct Termination<C: Fn()> {
    open: Cell<bool>,
    close: C,
}

impl<C: Fn()> Termination<C> {
    fn stop(&self) {
        if self.open.replace(false) {
            (self.close)();
        }
    }
}

impl<C: Fn()> Drop for Termination<C> {
    fn drop(&mut self) {
        self.stop();
    }
}

struct QueryGuard<'a, C: Fn()> {
    termination: &'a Termination<C>,
    armed: bool,
}

impl<C: Fn()> Drop for QueryGuard<'_, C> {
    fn drop(&mut self) {
        if self.armed {
            self.termination.stop();
        }
    }
}

/// Poll the driver only alongside this request's queries. Closing is an
/// idempotent synchronous initiation, not proof of physical closure. The Worker
/// adapter retains the currently active raw socket, including after STARTTLS.
/// No provider capacity or provider-close evidence is managed by this type.
pub struct DrivenRepository<R, D, C: Fn()> {
    repository: R,
    driver: Pin<Box<D>>,
    termination: Termination<C>,
}

impl<R, D, C: Fn()> DrivenRepository<R, D, C> {
    pub fn new(repository: R, driver: D, close: C) -> Self {
        Self {
            repository,
            driver: Box::pin(driver),
            termination: Termination {
                open: Cell::new(true),
                close,
            },
        }
    }
}

async fn drive<T, D, C>(
    mut driver: Pin<&mut D>,
    termination: &Termination<C>,
    query: impl Future<Output = Result<T, SourceServiceError>>,
) -> Result<T, SourceServiceError>
where
    D: Future<Output = Result<(), SourceServiceError>>,
    C: Fn(),
{
    if !termination.open.get() {
        return Err(SourceServiceError::Store);
    }
    let mut guard = QueryGuard {
        termination,
        armed: true,
    };
    let mut query = std::pin::pin!(query);
    let result = std::future::poll_fn(|cx| {
        // Register the driver's waker before the query can queue work. Driver
        // completion (even clean EOF) wins ties over a ready query result.
        if driver.as_mut().poll(cx).is_ready() {
            return Poll::Ready(Err(SourceServiceError::Store));
        }
        query.as_mut().poll(cx)
    })
    .await;
    if result.is_ok() {
        guard.armed = false;
    }
    result
}

impl<R, D, C> AsyncSourceRepository for DrivenRepository<R, D, C>
where
    R: AsyncSourceRepository,
    D: Future<Output = Result<(), SourceServiceError>>,
    C: Fn(),
{
    async fn metadata(
        &mut self,
        track_id: &str,
    ) -> Result<[Option<SourceMetadata>; 2], SourceServiceError> {
        drive(
            self.driver.as_mut(),
            &self.termination,
            self.repository.metadata(track_id),
        )
        .await
    }

    async fn envelope(
        &mut self,
        selected: &SourceMetadata,
    ) -> Result<Option<FieldEnvelope>, SourceServiceError> {
        drive(
            self.driver.as_mut(),
            &self.termination,
            self.repository.envelope(selected),
        )
        .await
    }
}

impl<R, D, C: Fn()> Drop for DrivenRepository<R, D, C> {
    fn drop(&mut self) {
        // Force-close before the client/driver's ordinary field destruction.
        self.termination.stop();
    }
}

#[cfg(test)]
mod tests;
