//! Both Container fetch sites are bounded by one 10 s deadline.
//!
//! PROTECTED acceptance spec: the implementation is built against this
//! file; this file is never edited to match an implementation.
//!
//! Gap under test (`workers/repair/src/runtime.rs`):
//!
//! - `stage_input` awaits `container.fetch_request(request)` and then
//!   `response.bytes()` with no deadline;
//! - `container_json` (the `/health`, `/cleanup` and `/repair`
//!   exchanges) awaits `port.fetch_request(request)` and then
//!   `response.bytes()` with no deadline.
//!
//! A hung or trickling Container therefore pends each call until some
//! coarse outer timeout, holding the job's Durable Object. The only
//! `Delay` in the file today is the 500 ms `/health` pacing sleep in
//! `container_port`, which bounds nothing.
//!
//! Contract the implementation must provide:
//!
//! - One named knob, next to `HEALTH_ATTEMPTS`:
//!   `const CONTAINER_FETCH_DEADLINE_MS: u64 = 10_000;`
//!   (the `*_MS: u64` shape the sibling ingest MB deadline uses).
//!   Both sites arm from this constant only — no inline
//!   `from_millis(10_000)` / `from_secs(10)` copies — so the budget is
//!   overridable in exactly one place.
//! - At EACH site the deadline is armed BEFORE the fetch is issued
//!   and spans headers AND the body read as one total: race the
//!   fetch+`bytes()` sequence against a `Delay::from(
//!   Duration::from_millis(CONTAINER_FETCH_DEADLINE_MS))` future
//!   (the codebase `std::future::poll_fn` race, deadline polled
//!   first, or a `select`), either inline at the site or in one
//!   shared helper both sites call. Partial progress never re-arms
//!   it, and a body still incomplete at expiry fails whole.
//! - Expiry maps to the EXISTING `ApiFault::new(502,
//!   "container_unavailable")` each site already returns for a
//!   failed fetch. No new fault code (`container_timeout`,
//!   `deadline_exceeded`, ...) may be introduced.
//! - The timer is a pinned local future dropped on every exit path
//!   (success, transport error, expiry): no leaked, forgotten or
//!   spawned timers.
//!
//! Out of scope: `fetch_output`'s R2 streaming fetch, the result-byte
//! cap values, and the 500 ms health-pacing sleep (all unchanged).
//!
//! Why source text: `runtime` compiles only for `wasm32`, so a native
//! integration target cannot call it; the pins read `runtime.rs` via
//! `include_str!` with comments and string literals stripped
//! (string-aware, so `"http://container/..."` URLs and comments
//! mentioning a deadline can neither satisfy nor break a pin), the
//! same technique as `health_budget_literals_acceptance`. This suite
//! therefore proves the wiring shape, not wasm behaviour; it does
//! not prove the Container was not mutated by a timed-out request.
//!
//! Run only this file:
//!
//! ```sh
//! cargo test --locked --manifest-path workers/repair/Cargo.toml \
//!     -p nntune-repair --test container_fetch_deadline
//! ```

/// `runtime.rs` source, read as text (the module is
/// `cfg(target_arch = "wasm32")` and unreachable from native tests).
const RUNTIME_RS: &str = include_str!("../src/runtime.rs");

const DEADLINE_CONST: &str = "CONTAINER_FETCH_DEADLINE_MS";
const RACE_TOKENS: [&str; 4] = ["poll_fn", "select!", "future::select", ".select("];

/// Blank out comments and string/char literal contents, preserving
/// newlines. String-aware: `//` inside a string literal (e.g. a
/// `"http://container/..."` URL) does not start a comment, and a
/// token mentioned only in a comment or string cannot satisfy a pin.
fn strip_comments_and_strings(src: &str) -> String {
    let chars: Vec<char> = src.chars().collect();
    let mut out: Vec<char> = Vec::with_capacity(chars.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied().unwrap_or('\0');
        if c == '/' && next == '/' {
            // Line comment: blank to (but not including) the newline.
            while i < chars.len() && chars[i] != '\n' {
                out.push(' ');
                i += 1;
            }
        } else if c == '/' && next == '*' {
            // Block comment (Rust nests these): blank to the match.
            let mut depth = 0usize;
            while i < chars.len() {
                if chars[i] == '/' && chars.get(i + 1) == Some(&'*') {
                    depth += 1;
                    out.extend([' ', ' ']);
                    i += 2;
                } else if chars[i] == '*' && chars.get(i + 1) == Some(&'/') {
                    depth -= 1;
                    out.extend([' ', ' ']);
                    i += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    out.push(if chars[i] == '\n' { '\n' } else { ' ' });
                    i += 1;
                }
            }
        } else if c == '"' {
            // String literal: blank contents, honouring escapes.
            out.push(' ');
            i += 1;
            while i < chars.len() {
                match chars[i] {
                    '\\' => {
                        out.extend([' ', ' ']);
                        i += 2;
                    }
                    '"' => {
                        out.push(' ');
                        i += 1;
                        break;
                    }
                    other => {
                        out.push(if other == '\n' { '\n' } else { ' ' });
                        i += 1;
                    }
                }
            }
        } else if c == '\'' && (next == '\\' || chars.get(i + 2) == Some(&'\'')) {
            // Char literal (lifetimes like `'static` fall through):
            // blank to the closing quote, honouring escapes.
            out.push(' ');
            i += 1;
            while i < chars.len() {
                match chars[i] {
                    '\\' => {
                        out.extend([' ', ' ']);
                        i += 2;
                    }
                    '\'' => {
                        out.push(' ');
                        i += 1;
                        break;
                    }
                    _ => {
                        out.push(' ');
                        i += 1;
                    }
                }
            }
        } else {
            out.push(c);
            i += 1;
        }
    }
    out.into_iter().collect()
}

/// `runtime.rs` with comments and literals stripped: the code pins
/// below read only this view.
fn code() -> String {
    strip_comments_and_strings(RUNTIME_RS)
}

/// Slice of one function, bounded by the next named function.
fn fn_slice<'a>(src: &'a str, start: &str, next: &str) -> &'a str {
    let after = src
        .split_once(start)
        .unwrap_or_else(|| panic!("{start} must exist in runtime.rs"))
        .1;
    after
        .split_once(next)
        .unwrap_or_else(|| panic!("{next} must bound {start} in runtime.rs"))
        .0
}

fn stage_input_code(code: &str) -> &str {
    fn_slice(code, "async fn stage_input", "async fn fetch_output")
}

fn container_json_code(code: &str) -> &str {
    fn_slice(
        code,
        "async fn container_json",
        "async fn read_object_capped",
    )
}

/// Every `fn name` definition with the source segment up to the next
/// definition, so a shared deadline helper can be recognised by what
/// its own body contains rather than by a pinned helper name.
fn fn_defs(code: &str) -> Vec<(String, &str)> {
    let mut starts: Vec<(usize, String)> = Vec::new();
    for (idx, _) in code.match_indices("fn ") {
        let name: String = code[idx + 3..]
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        if !name.is_empty() {
            starts.push((idx, name));
        }
    }
    starts
        .iter()
        .enumerate()
        .map(|(n, (idx, name))| {
            let end = starts
                .get(n + 1)
                .map(|(next, _)| *next)
                .unwrap_or(code.len());
            (name.clone(), &code[*idx..end])
        })
        .collect()
}

/// Names of functions whose own body arms the named deadline and
/// races it: calling one bounds a fetch exactly as an inline race
/// would. (The health pacing sleep qualifies on neither count: it
/// references no deadline constant and races nothing.)
fn deadline_helpers(code: &str) -> Vec<String> {
    fn_defs(code)
        .into_iter()
        .filter(|(_, body)| {
            body.contains(DEADLINE_CONST)
                && body.contains("Delay::from")
                && RACE_TOKENS.iter().any(|t| body.contains(t))
        })
        .map(|(name, _)| name)
        .collect()
}

fn helper_call_positions(slice: &str, code: &str) -> Vec<usize> {
    deadline_helpers(code)
        .iter()
        .flat_map(|name| {
            slice
                .match_indices(&format!("{name}("))
                .map(|(idx, _)| idx)
                .collect::<Vec<_>>()
        })
        .collect()
}

/// The site races its fetch against the named deadline, inline or
/// through a shared helper that does.
fn site_races_deadline(slice: &str, code: &str) -> bool {
    let inline = slice.contains(DEADLINE_CONST)
        && slice.contains("Delay::from")
        && RACE_TOKENS.iter().any(|t| slice.contains(t));
    inline || !helper_call_positions(slice, code).is_empty()
}

/// The deadline (constant, timer, or helper call) is armed textually
/// before `fetch_request(` is issued, so slow headers count against
/// the same total as the body.
fn armed_before_fetch(slice: &str, code: &str) -> bool {
    let Some(fetch_at) = slice.find("fetch_request(") else {
        return false;
    };
    let mut arms: Vec<usize> = [DEADLINE_CONST, "Delay::from"]
        .iter()
        .filter_map(|t| slice.find(t))
        .collect();
    arms.extend(helper_call_positions(slice, code));
    arms.iter().any(|arm| *arm < fetch_at)
}

/// The body read is inside the bound, not a bare await after a
/// headers-only race. Accepts: the `.bytes()` future is constructed
/// inside work that a later race polls; a helper wrapping an
/// `async` block containing `.bytes()`; or a helper invoked
/// separately for headers and body.
fn body_covered(slice: &str, code: &str) -> bool {
    let Some(bytes_at) = slice.find(".bytes()") else {
        return false;
    };
    let helpers = helper_call_positions(slice, code);
    let mut tokens: Vec<usize> = RACE_TOKENS
        .iter()
        .flat_map(|t| {
            slice
                .match_indices(t)
                .map(|(idx, _)| idx)
                .collect::<Vec<_>>()
        })
        .collect();
    tokens.extend(helpers.iter().copied());
    if tokens.iter().any(|t| *t > bytes_at) {
        return true;
    }
    if let Some(async_at) = slice.find("async") {
        if async_at < bytes_at && helpers.iter().any(|h| *h < async_at) {
            return true;
        }
    }
    helpers.len() >= 2
}

// ---------------------------------------------------------------------------
// Controls: adjacent behaviour that must stay green before and after.
// ---------------------------------------------------------------------------

#[test]
fn stripper_preserves_code_after_slashes_in_strings() {
    // The trap this suite must survive: `//` inside a URL string is
    // not a comment, and deadline tokens inside strings or comments
    // must not satisfy the pins below.
    let sample = "let u = \"https://example//Delay::from\"; // Delay::from poll_fn\n\
                  fetch_request(request)";
    let stripped = strip_comments_and_strings(sample);
    assert!(
        stripped.contains("fetch_request(request)"),
        "code after a `//` inside a string literal must survive stripping"
    );
    assert!(
        !stripped.contains("Delay::from") && !stripped.contains("poll_fn"),
        "tokens inside strings/comments must be blanked, got: {stripped}"
    );
}

#[test]
fn stage_input_still_puts_the_staged_object_via_fetch_request() {
    let code = code();
    let slice = stage_input_code(&code);
    assert!(
        slice.contains("Method::Put") && slice.contains("fetch_request("),
        "stage_input must keep PUT-ing the staged object through the Container fetcher"
    );
    assert!(
        slice.contains("201 =>"),
        "stage_input must keep accepting only the Container's 201"
    );
    assert!(
        slice.contains(".bytes()"),
        "stage_input must keep draining the reply body"
    );
}

#[test]
fn container_json_still_caps_the_reply_bytes() {
    let code = code();
    let slice = container_json_code(&code);
    assert!(
        slice.contains("fetch_request(") && slice.contains(".bytes()"),
        "container_json must keep exchanging through the port fetcher and reading the body"
    );
    assert!(
        slice.contains("> MAX_RESULT_BYTES"),
        "container_json must keep rejecting replies over MAX_RESULT_BYTES"
    );
}

#[test]
fn health_pacing_sleep_is_unchanged() {
    let code = code();
    let slice = fn_slice(&code, "async fn container_port", "async fn stage_input");
    assert!(
        slice.contains("Delay::from(Duration::from_millis(500))"),
        "the 500 ms /health pacing sleep must stay (it is pacing, not the fetch deadline)"
    );
    assert!(
        slice.contains("for _ in 0..HEALTH_ATTEMPTS"),
        "the /health poll loop bound must stay HEALTH_ATTEMPTS"
    );
}

#[test]
fn no_new_fault_code_for_a_fetch_expiry() {
    // Expiry reuses the fault each site already returns today; a new
    // timeout-specific code would silently change upstream mapping.
    for code_name in [
        "\"container_timeout\"",
        "\"fetch_timeout\"",
        "\"container_deadline\"",
        "\"deadline_exceeded\"",
        "\"fetch_deadline\"",
    ] {
        assert!(
            !RUNTIME_RS.contains(code_name),
            "runtime.rs must not introduce the new fault code {code_name}"
        );
    }
    let stage = fn_slice(RUNTIME_RS, "async fn stage_input", "async fn fetch_output");
    let json = fn_slice(
        RUNTIME_RS,
        "async fn container_json",
        "async fn read_object_capped",
    );
    assert!(
        stage.contains("502, \"container_unavailable\""),
        "stage_input must keep failing a Container call as 502 container_unavailable"
    );
    assert!(
        json.contains("502, \"container_unavailable\""),
        "container_json must keep failing a Container call as 502 container_unavailable"
    );
}

#[test]
fn timers_are_not_leaked_forgotten_or_spawned() {
    let code = code();
    for banned in ["mem::forget", "Box::leak", "spawn_local"] {
        assert!(
            !code.contains(banned),
            "runtime.rs must not {banned}: the deadline future is a pinned local dropped on every exit"
        );
    }
}

// ---------------------------------------------------------------------------
// The deadline itself: RED on the source this suite was written
// against (neither site races a timer), green once implemented.
// ---------------------------------------------------------------------------

#[test]
fn deadline_constant_is_one_named_10_second_knob() {
    let code = code();
    assert!(
        code.contains("const CONTAINER_FETCH_DEADLINE_MS: u64 = 10_000;"),
        "runtime.rs must declare the single overridable knob \
         `const CONTAINER_FETCH_DEADLINE_MS: u64 = 10_000;` (10 s)"
    );
    for (name, slice) in [
        ("stage_input", stage_input_code(&code)),
        ("container_json", container_json_code(&code)),
    ] {
        assert!(
            !slice.contains("Duration::from_millis(10_000)")
                && !slice.contains("Duration::from_secs(10)"),
            "{name} must arm from CONTAINER_FETCH_DEADLINE_MS, not an inline 10 s copy"
        );
    }
}

#[test]
fn stage_input_races_its_container_fetch_against_the_deadline() {
    let code = code();
    let slice = stage_input_code(&code);
    assert!(
        site_races_deadline(slice, &code),
        "stage_input must race `container.fetch_request(request)` against a \
         Delay armed from CONTAINER_FETCH_DEADLINE_MS (inline or via a shared \
         helper); today it awaits the fetch unbounded"
    );
    assert!(
        armed_before_fetch(slice, &code),
        "stage_input must arm the deadline before issuing fetch_request, so \
         slow headers count against the same 10 s total"
    );
}

#[test]
fn container_json_races_its_container_fetch_against_the_deadline() {
    let code = code();
    let slice = container_json_code(&code);
    assert!(
        site_races_deadline(slice, &code),
        "container_json must race `port.fetch_request(request)` against a \
         Delay armed from CONTAINER_FETCH_DEADLINE_MS (inline or via a shared \
         helper); today it awaits the fetch unbounded"
    );
    assert!(
        armed_before_fetch(slice, &code),
        "container_json must arm the deadline before issuing fetch_request, so \
         slow headers count against the same 10 s total"
    );
}

#[test]
fn deadline_at_both_sites_covers_the_body_read_not_only_headers() {
    let code = code();
    assert!(
        body_covered(stage_input_code(&code), &code),
        "stage_input's `response.bytes()` drain must be inside the deadline, \
         not a bare await after a headers-only race"
    );
    assert!(
        body_covered(container_json_code(&code), &code),
        "container_json's `response.bytes()` read must be inside the deadline, \
         not a bare await after a headers-only race"
    );
}
