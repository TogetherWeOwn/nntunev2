//! Container cold-start budget: 60 `/health` polls 500 ms apart
//! (30 s total) before the exchange fails closed.
//!
//! `runtime` compiles only for `wasm32`, so these pins read its
//! source (normalised) instead of calling it.

const RUNTIME_RS: &str = include_str!("../src/runtime.rs");

fn normalized() -> String {
    RUNTIME_RS.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[test]
fn health_poll_budget_is_sixty_attempts_of_half_a_second() {
    let text = normalized();
    assert!(
        text.contains("const HEALTH_ATTEMPTS: u32 = 60;"),
        "cold-start attempts must stay a single named knob at 60"
    );
    assert!(
        text.contains("for _ in 0..HEALTH_ATTEMPTS"),
        "container_port must poll exactly HEALTH_ATTEMPTS times"
    );
    assert!(
        text.contains("Delay::from(Duration::from_millis(500))"),
        "health polls must wait 500 ms apart"
    );
}
