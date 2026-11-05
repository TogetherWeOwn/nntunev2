//! Native scheduled-entry policy; no Worker, Secret, mirror or database.

use nntune_api::videolinks::jobs::{JobEnvironment, JobError, RetentionTick};
use nntune_api::videolinks::scheduled::{run_retention, ScheduleError, RETENTION_CRON};
use std::cell::Cell;
use std::future::Future;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

const TICK_MS: f64 = 1_800_000_000_000.0;
const NOW_MS: u64 = 1_800_000_005_000;
const SHA: &str = "0123456789abcdef0123456789abcdef01234567";

fn vars(name: &str) -> Option<String> {
    match name {
        "ENVIRONMENT" => Some("staging".into()),
        "BUILD_SHA" => Some(SHA.into()),
        "HYPERDRIVE_CACHING_DISABLED" => Some("1".into()),
        _ => panic!("unexpected configuration lookup: {name}"),
    }
}

fn finish<F: Future>(future: F) -> F::Output {
    let mut future = std::pin::pin!(future);
    let end = Instant::now() + Duration::from_secs(2);
    let mut cx = Context::from_waker(std::task::Waker::noop());
    loop {
        if let Poll::Ready(value) = future.as_mut().poll(&mut cx) {
            return value;
        }
        assert!(Instant::now() < end, "scheduled seam exceeded watchdog");
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn retention_uses_only_minimal_public_configuration_and_runs_once() {
    for (label, expected) in [
        ("staging", JobEnvironment::Staging),
        ("production", JobEnvironment::Production),
    ] {
        let calls = Cell::new(0);
        let result = finish(run_retention(
            RETENTION_CRON,
            TICK_MS,
            NOW_MS,
            |name| {
                if name == "ENVIRONMENT" {
                    Some(label.into())
                } else {
                    vars(name)
                }
            },
            |invocation| {
                calls.set(calls.get() + 1);
                assert_eq!(invocation.environment, expected);
                assert_eq!(invocation.scheduled_at, 1_800_000_000);
                assert_eq!(invocation.build_sha, SHA);
                async { Ok(RetentionTick::Suppressed) }
            },
        ));
        assert_eq!(result, Ok(RetentionTick::Suppressed));
        assert_eq!(calls.get(), 1);
    }
}

#[test]
fn unknown_cron_and_invalid_event_clocks_never_lookup_config_or_connect() {
    for cron in ["", "7,22,37,52 * * * *", "* * * * *", " */15 * * * *"] {
        assert_eq!(
            finish(run_retention(
                cron,
                TICK_MS,
                NOW_MS,
                |_| panic!("lookup"),
                |_| async { panic!("connect") }
            )),
            Err(ScheduleError::UnknownCron)
        );
    }
    for tick in [
        f64::NAN,
        f64::INFINITY,
        f64::NEG_INFINITY,
        -900_000.0,
        0.0,
        TICK_MS + 0.5,
        TICK_MS + 1000.0,
        TICK_MS + 900_000.0,
        9_007_199_254_740_992.0,
    ] {
        assert_eq!(
            finish(run_retention(
                RETENTION_CRON,
                tick,
                NOW_MS,
                |_| panic!("lookup"),
                |_| async { panic!("connect") }
            )),
            Err(ScheduleError::InvalidTime)
        );
    }
    assert_eq!(
        finish(run_retention(RETENTION_CRON, TICK_MS, 0, vars, |_| async {
            panic!("connect")
        })),
        Err(ScheduleError::InvalidTime)
    );
}

#[test]
fn missing_or_invalid_minimal_configuration_never_connects() {
    for (key, bad) in [
        ("ENVIRONMENT", None),
        ("ENVIRONMENT", Some("")),
        ("ENVIRONMENT", Some("prod")),
        ("ENVIRONMENT", Some("preview")),
        ("BUILD_SHA", None),
        ("BUILD_SHA", Some("unknown")),
        (
            "BUILD_SHA",
            Some("0123456789ABCDEF0123456789abcdef01234567"),
        ),
        ("HYPERDRIVE_CACHING_DISABLED", None),
        ("HYPERDRIVE_CACHING_DISABLED", Some("0")),
    ] {
        assert_eq!(
            finish(run_retention(
                RETENTION_CRON,
                TICK_MS,
                NOW_MS,
                |name| if name == key {
                    bad.map(str::to_owned)
                } else {
                    vars(name)
                },
                |_| async { panic!("invalid config reached connection acquisition") },
            )),
            Err(ScheduleError::Configuration),
            "{key} {bad:?}"
        );
    }
}

/// Structural regression pin only: native fixtures cannot exercise JS Promise
/// rejection. The pinned scheduled macro discards Result; throw_str skips the
/// async bridge's completion. Worker qualification must still test the export.
#[test]
fn scheduled_export_returns_awaited_result_without_throwing_from_poll() {
    let source = include_str!("../src/lib.rs");
    let export = source
        .split_once("mod scheduled_export {")
        .expect("explicit scheduled Promise export")
        .1
        .split_once("/// True when this request")
        .expect("end of scheduled export")
        .0;
    assert!(export.contains("-> Result<JsValue, JsValue>"));
    assert!(export.contains("dispatch_scheduled(event.into(), env)"));
    assert!(export.contains(".await"));
    assert!(export.contains(".map_err(|label| JsValue::from_str(label))"));
    assert!(!export.contains("throw_str"));
    assert!(!source.contains("#[worker::event(scheduled)]"));
}

#[test]
fn connection_and_job_failures_propagate_without_retry_or_success() {
    for error in [
        ScheduleError::Connect,
        ScheduleError::Job(JobError::Store),
        ScheduleError::Job(JobError::Deadline),
        ScheduleError::Job(JobError::LostLease),
    ] {
        let calls = Cell::new(0);
        let result = finish(run_retention(RETENTION_CRON, TICK_MS, NOW_MS, vars, |_| {
            calls.set(calls.get() + 1);
            async { Err(error) }
        }));
        assert_eq!(result, Err(error));
        assert_eq!(calls.get(), 1);
    }
}
