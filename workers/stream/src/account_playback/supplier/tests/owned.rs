//! Pool-owned static inputs only. These do not claim a scheduled HTTP response,
//! writer cleanup, remote physical closure or actual Worker qualification.

use super::*;
use std::rc::Weak;

struct Probe<T> {
    inner: T,
    _lease: Rc<()>,
}

impl<T: RegistrySource> RegistrySource for Probe<T> {
    async fn fetch(&self, authorization: &str) -> Result<Vec<u8>, RegistryError> {
        self.inner.fetch(authorization).await
    }
}

impl<T: AccountSource> AccountSource for Probe<T> {
    async fn candidate(
        &self,
        account_id: &str,
        authorization: &str,
        body: &[u8],
    ) -> Result<Vec<u8>, RegistryError> {
        self.inner.candidate(account_id, authorization, body).await
    }

    async fn execute(&self, account_id: &str, authorization: &str, body: &[u8]) -> OperationReply {
        self.inner.execute(account_id, authorization, body).await
    }
}

struct Witness {
    registry: Weak<()>,
    accounts: Weak<()>,
    clock: Weak<Cell<u64>>,
    timer: Weak<()>,
    abort: Rc<SignalState>,
    request_timer: Rc<SignalState>,
    operation_timer: Rc<SignalState>,
    pending_catalogue: Rc<Cell<bool>>,
    pending_registry: Rc<Cell<bool>>,
    pending_body: Rc<Cell<bool>>,
}

impl Witness {
    fn assert_released(&self) {
        assert!(self.registry.upgrade().is_none());
        assert!(self.accounts.upgrade().is_none());
        assert!(self.clock.upgrade().is_none());
        assert!(self.timer.upgrade().is_none());
        assert!(self.abort.dropped.get());
        assert!(self.request_timer.dropped.get());
    }
}

struct OwnedCase {
    catalogue: FakeCatalogue,
    inputs: OwnedSupplierInputs<Probe<FakeRegistry>, Probe<FakeAccounts>>,
    control: RequestControl<'static>,
    witness: Witness,
}

impl OwnedCase {
    fn new(rig: Rig) -> Self {
        let registry_lease = Rc::new(());
        let account_lease = Rc::new(());
        let timer_lease = Rc::new(());
        let witness = Witness {
            registry: Rc::downgrade(&registry_lease),
            accounts: Rc::downgrade(&account_lease),
            clock: Rc::downgrade(&rig.now),
            timer: Rc::downgrade(&timer_lease),
            abort: rig.abort.clone(),
            request_timer: rig.req_timer.clone(),
            operation_timer: rig.op_timer.clone(),
            pending_catalogue: rig.catalogue.dropped.clone(),
            pending_registry: rig.registry.dropped.clone(),
            pending_body: rig.accounts.execute_dropped.clone(),
        };
        let token = grant::issue_v2("user", TRACK, "play", 1, GRANT_EXP, GRANT_KEY);
        let now = rig.now;
        let control = RequestControl::with_clock(
            &token,
            TRACK,
            GRANT_KEY,
            Rc::new(move || now.get()),
            NOW + 120_000,
            signal(&rig.abort),
            |_| signal(&rig.req_timer),
        )
        .unwrap();
        let operation_signal = rig.op_timer;
        Self {
            catalogue: rig.catalogue,
            inputs: OwnedSupplierInputs {
                registry: Probe {
                    inner: rig.registry,
                    _lease: registry_lease,
                },
                accounts: Probe {
                    inner: rig.accounts,
                    _lease: account_lease,
                },
                account_key: Zeroizing::new(ACCOUNT_KEY.to_vec()),
                resolver_key: Zeroizing::new(RESOLVER_KEY.to_vec()),
                operation_timer: Box::new(move |remaining| {
                    let _retain = &timer_lease;
                    assert!(remaining > 0 && remaining <= SESSION_TTL_MS);
                    signal(&operation_signal)
                }),
            },
            control,
            witness,
        }
    }
}

fn static_future<T>(
    future: impl Future<Output = T> + 'static,
) -> impl Future<Output = T> + 'static {
    future
}

#[test]
fn unpolled_owned_preparation_releases_every_runtime_input() {
    let OwnedCase {
        catalogue,
        inputs,
        control,
        witness,
    } = OwnedCase::new(Rig::with_rows(1, &[(1, 0)]));
    let task = static_future(ArticleSupplier::prepare_owned(catalogue, inputs, control));
    assert!(witness.registry.upgrade().is_some());
    assert!(witness.clock.upgrade().is_some());
    drop(task);
    witness.assert_released();
    // Lookup and operation timers were never polled/constructed.
    assert!(!witness.pending_catalogue.get());
    assert!(!witness.operation_timer.dropped.get());
}

#[test]
fn owned_supplier_shares_one_source_allocation_and_releases_dependencies() {
    let OwnedCase {
        catalogue,
        inputs,
        control,
        witness,
    } = OwnedCase::new(Rig::with_rows(1, &[(1, 0)]));
    let mut supplier = ready(static_future(ArticleSupplier::prepare_owned(
        catalogue, inputs, control,
    )))
    .unwrap();
    let source = supplier.shared_source();
    let second = supplier.shared_source();
    assert!(Rc::ptr_eq(&source, &second));
    assert!(std::ptr::eq(source.as_ref(), supplier.source()));
    assert_eq!(Rc::strong_count(&source), 3);
    assert!(!ready(supplier.open(0, &reference(0))).unwrap().is_empty());
    assert_eq!(supplier.serving_accounts(), &[uid(1)]);
    let source_weak = Rc::downgrade(&source);
    drop(second);
    drop(supplier);
    witness.assert_released();
    assert_eq!(Rc::strong_count(&source), 1);
    drop(source);
    assert!(source_weak.upgrade().is_none());
}

#[test]
fn pending_owned_catalogue_drop_releases_inputs_and_poisons_same_control() {
    let rig = Rig::with_rows(1, &[(1, 0)]);
    *rig.catalogue.mode.borrow_mut() = CatMode::Pending;
    let OwnedCase {
        catalogue,
        inputs,
        control,
        witness,
    } = OwnedCase::new(rig);
    let observer = control.clone();
    let mut task = Box::pin(static_future(ArticleSupplier::prepare_owned(
        catalogue, inputs, control,
    )));
    assert!(poll(&mut task, Waker::noop()).is_pending());
    drop(task);
    assert!(witness.pending_catalogue.get());
    assert_eq!(observer.check_live(), Err(SourceError::Cancelled));
    drop(observer);
    witness.assert_released();
}

#[test]
fn static_outer_task_drop_cancels_pending_registry_and_releases_inputs() {
    let rig = Rig::with_rows(1, &[(1, 0)]);
    rig.registry.pending.set(true);
    let OwnedCase {
        catalogue,
        inputs,
        control,
        witness,
    } = OwnedCase::new(rig);
    let observer = control.clone();
    let mut task = Box::pin(static_future(async move {
        let mut supplier = ArticleSupplier::prepare_owned(catalogue, inputs, control).await?;
        supplier.open(0, &reference(0)).await
    }));
    assert!(poll(&mut task, Waker::noop()).is_pending());
    drop(task);
    assert!(witness.pending_registry.get());
    assert!(witness.operation_timer.dropped.get());
    assert_eq!(observer.check_live(), Err(SourceError::Cancelled));
    drop(observer);
    witness.assert_released();
}

#[test]
fn owned_pending_body_cancels_under_same_authority_and_releases_inputs() {
    let rig = Rig::with_rows(1, &[(1, 0)]);
    rig.accounts.execute_pending.set(true);
    let OwnedCase {
        catalogue,
        inputs,
        control,
        witness,
    } = OwnedCase::new(rig);
    let observer = control.clone();
    let mut task = Box::pin(static_future(async move {
        let mut supplier = ArticleSupplier::prepare_owned(catalogue, inputs, control).await?;
        supplier.open(0, &reference(0)).await
    }));
    assert!(poll(&mut task, Waker::noop()).is_pending());
    witness.abort.fire();
    assert_eq!(
        poll(&mut task, Waker::noop()),
        Poll::Ready(Err(SourceError::Cancelled))
    );
    drop(task);
    assert!(witness.pending_body.get());
    assert!(witness.operation_timer.dropped.get());
    assert_eq!(observer.check_live(), Err(SourceError::Cancelled));
    drop(observer);
    witness.assert_released();
}

#[test]
fn owned_short_key_refuses_before_catalogue_or_account_work() {
    let rig = Rig::with_rows(1, &[(1, 0)]);
    *rig.catalogue.mode.borrow_mut() = CatMode::Pending;
    let OwnedCase {
        catalogue,
        mut inputs,
        control,
        witness,
    } = OwnedCase::new(rig);
    inputs.account_key = Zeroizing::new(vec![5; 31]);
    let result = ready(static_future(ArticleSupplier::prepare_owned(
        catalogue, inputs, control,
    )));
    assert!(matches!(result, Err(SourceError::Transport)));
    assert!(!witness.pending_catalogue.get());
    assert!(!witness.operation_timer.dropped.get());
    witness.assert_released();
}
