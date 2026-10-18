//! Pool route-decision matrix: refusals happen before any dial.
//!
//! Pins `decide_pool_action` over method x path x owner-name: wrong method,
//! unknown path, owner-less right route (refused before any dial), and the
//! two named-route successes (`POST /probe` -> `RunProbe`,
//! `POST /body` -> `RunBody`). Every refusal yields `Respond(..)` and never
//! `RunProbe`/`RunBody`; the three `ProbeDecision` renderings (status/body)
//! are pinned so a swapped matrix or a `503 -> 200` mutant turns this suite
//! red. Pure calls only: no sockets, providers, or Durable Objects.

use nntune_stream::routes::{decide_pool_action, PoolAction, ProbeDecision};

/// One matrix cell: input triple plus the exact expected action.
struct Cell {
    method: &'static str,
    path: &'static str,
    has_owner: bool,
    expected: PoolAction,
}

fn matrix() -> Vec<Cell> {
    vec![
        // Named routes with an owner name: the only cells that run.
        Cell {
            method: "POST",
            path: "/probe",
            has_owner: true,
            expected: PoolAction::RunProbe,
        },
        Cell {
            method: "POST",
            path: "/body",
            has_owner: true,
            expected: PoolAction::RunBody,
        },
        // Owner-less right route: refused before any dial.
        Cell {
            method: "POST",
            path: "/probe",
            has_owner: false,
            expected: PoolAction::Respond(ProbeDecision::OwnerMissing),
        },
        Cell {
            method: "POST",
            path: "/body",
            has_owner: false,
            expected: PoolAction::Respond(ProbeDecision::OwnerMissing),
        },
        // Wrong method on right paths: 404 even with (and without) owner.
        // Method/path is checked before ownership, so an owner-less
        // wrong-method request is NotFound, not OwnerMissing.
        Cell {
            method: "GET",
            path: "/probe",
            has_owner: true,
            expected: PoolAction::Respond(ProbeDecision::NotFound),
        },
        Cell {
            method: "GET",
            path: "/probe",
            has_owner: false,
            expected: PoolAction::Respond(ProbeDecision::NotFound),
        },
        Cell {
            method: "GET",
            path: "/body",
            has_owner: true,
            expected: PoolAction::Respond(ProbeDecision::NotFound),
        },
        Cell {
            method: "GET",
            path: "/body",
            has_owner: false,
            expected: PoolAction::Respond(ProbeDecision::NotFound),
        },
        Cell {
            method: "PUT",
            path: "/probe",
            has_owner: true,
            expected: PoolAction::Respond(ProbeDecision::NotFound),
        },
        Cell {
            method: "DELETE",
            path: "/body",
            has_owner: true,
            expected: PoolAction::Respond(ProbeDecision::NotFound),
        },
        Cell {
            method: "post",
            path: "/probe",
            has_owner: true,
            expected: PoolAction::Respond(ProbeDecision::NotFound),
        },
        Cell {
            method: "",
            path: "",
            has_owner: true,
            expected: PoolAction::Respond(ProbeDecision::NotFound),
        },
        // Unknown path with the right method: 404 either way.
        Cell {
            method: "POST",
            path: "/",
            has_owner: true,
            expected: PoolAction::Respond(ProbeDecision::NotFound),
        },
        Cell {
            method: "POST",
            path: "/other",
            has_owner: true,
            expected: PoolAction::Respond(ProbeDecision::NotFound),
        },
        Cell {
            method: "POST",
            path: "/other",
            has_owner: false,
            expected: PoolAction::Respond(ProbeDecision::NotFound),
        },
        Cell {
            method: "POST",
            path: "/probe/",
            has_owner: true,
            expected: PoolAction::Respond(ProbeDecision::NotFound),
        },
        Cell {
            method: "POST",
            path: "/body/",
            has_owner: true,
            expected: PoolAction::Respond(ProbeDecision::NotFound),
        },
        Cell {
            method: "POST",
            path: "/PROBE",
            has_owner: true,
            expected: PoolAction::Respond(ProbeDecision::NotFound),
        },
        Cell {
            method: "POST",
            path: "/BODY",
            has_owner: true,
            expected: PoolAction::Respond(ProbeDecision::NotFound),
        },
    ]
}

#[test]
fn pool_route_matrix_pins_every_cell() {
    for cell in matrix() {
        let action = decide_pool_action(cell.method, cell.path, cell.has_owner);
        assert_eq!(
            action, cell.expected,
            "{} {} owner={}",
            cell.method, cell.path, cell.has_owner
        );
    }
}

#[test]
fn refusals_yield_respond_never_run() {
    for cell in matrix() {
        let action = decide_pool_action(cell.method, cell.path, cell.has_owner);
        match cell.expected {
            PoolAction::Respond(_) => assert!(
                matches!(action, PoolAction::Respond(_)),
                "{} {} owner={}: refusal must be Respond, got {action:?}",
                cell.method,
                cell.path,
                cell.has_owner
            ),
            PoolAction::RunProbe => assert_eq!(action, PoolAction::RunProbe),
            PoolAction::RunBody => assert_eq!(action, PoolAction::RunBody),
        }
        // No refusal cell may select either run variant.
        if matches!(cell.expected, PoolAction::Respond(_)) {
            assert!(
                !matches!(action, PoolAction::RunProbe | PoolAction::RunBody),
                "{} {} owner={}: refusal must never run",
                cell.method,
                cell.path,
                cell.has_owner
            );
        }
    }
}

#[test]
fn owner_less_right_route_is_refused_before_any_dial() {
    // `decide_pool_action` takes no dial handle: a `Respond` decision leaves
    // nothing to dial with. Model the gate the adapter must honor: the dial
    // closure panics if it is ever called for a refusal.
    let dial = || panic!("refused request must never dial");
    for path in ["/probe", "/body"] {
        let action = decide_pool_action("POST", path, false);
        assert_eq!(
            action,
            PoolAction::Respond(ProbeDecision::OwnerMissing),
            "POST {path} without owner"
        );
        match action {
            PoolAction::Respond(decision) => {
                assert_eq!(decision.status(), 400);
                assert_eq!(decision.body(), r#"{"error":"bad_request"}"#);
            }
            PoolAction::RunProbe | PoolAction::RunBody => dial(),
        }
    }
}

#[test]
fn refusal_rendering_pins_status_and_body() {
    assert_eq!(ProbeDecision::NotFound.status(), 404);
    assert_eq!(ProbeDecision::NotFound.body(), "not found");
    assert_eq!(ProbeDecision::OwnerMissing.status(), 400);
    assert_eq!(
        ProbeDecision::OwnerMissing.body(),
        r#"{"error":"bad_request"}"#
    );
    // Pinned even though `decide_pool_action` never returns this variant
    // today: the adapter renders it for named probes until provider wiring
    // lands, and the `503 -> 200` mutant must stay red.
    assert_eq!(ProbeDecision::ProviderUnavailable.status(), 503);
    assert_ne!(
        ProbeDecision::ProviderUnavailable.status(),
        200,
        "provider wiring is absent: never 200"
    );
    assert_eq!(
        ProbeDecision::ProviderUnavailable.body(),
        r#"{"error":"provider_unavailable"}"#
    );
}

#[test]
fn matrix_refusals_render_through_their_decision() {
    for cell in matrix() {
        let PoolAction::Respond(decision) =
            decide_pool_action(cell.method, cell.path, cell.has_owner)
        else {
            continue;
        };
        let (status, body) = (decision.status(), decision.body());
        match decision {
            ProbeDecision::NotFound => {
                assert_eq!(status, 404, "{} {}", cell.method, cell.path);
                assert_eq!(body, "not found", "{} {}", cell.method, cell.path);
            }
            ProbeDecision::OwnerMissing => {
                assert_eq!(status, 400, "{} {}", cell.method, cell.path);
                assert_eq!(
                    body, r#"{"error":"bad_request"}"#,
                    "{} {}",
                    cell.method, cell.path
                );
            }
            ProbeDecision::ProviderUnavailable => {
                panic!(
                    "pool matrix never refuses with ProviderUnavailable: {} {}",
                    cell.method, cell.path
                );
            }
        }
    }
}
