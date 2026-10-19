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
//!   and spans headers AND the body handling as one total: race the
//!   fetch against a `Delay::from(
//!   Duration::from_millis(CONTAINER_FETCH_DEADLINE_MS))` future
//!   (the codebase `std::future::poll_fn` race, deadline polled
//!   first, or a `select`), either inline at the site or in one
//!   shared helper both sites call. Partial progress never re-arms
//!   it, and a body still incomplete at expiry fails whole.
//!   `stage_input` never reads its PUT reply (it cancels the
//!   unread body after the race); `container_json` reads its
//!   streamed reply under the same deadline.
//! - Expiry maps to the EXISTING `ApiFault::new(502,
//!   "container_unavailable")` each site already returns for a
//!   failed fetch. No new fault code (`container_timeout`,
//!   `deadline_exceeded`, ...) may be introduced.
//! - The timer is a pinned local future dropped on every exit path
//!   (success, transport error, expiry): no leaked, forgotten or
//!   spawned timers.
//!
//! Scoped follow-up semantics, now pinned per site (not just the flat
//! 10 s shape above):
//!
//! - Each site derives a `budget_ms` and arms exactly one
//!   `Duration::from_millis(budget_ms)` from it — never
//!   `from_secs(budget_ms)`, which would be 1000x the budget.
//! - `container_json` selects the budget by path: `path == "/repair"`
//!   arms `CONTAINER_REPAIR_DEADLINE_MS` (`150_000`, covering the
//!   Container's 120 s par2 budget), every other path keeps
//!   `CONTAINER_FETCH_DEADLINE_MS`.
//! - `stage_input` and `fetch_output` scale with transfer size atop
//!   the 10 s base (`size.saturating_div(1024)` ms, a ~1 MB/s floor)
//!   so a large healthy transfer is not expired mid-stream.
//! - `stage_input` never reads its reply body: it cancels the
//!   streamed body (`let _ = stream.cancel();`, the result ignored)
//!   after the fetch race, so no drain transport error can override
//!   the verdict — only the fetch race itself (expiry or transport
//!   failure) fails as 502, and a stored 201 and a known 400/413
//!   keep their mapping.
//! - `container_json` reads its reply as a raced stream: chunks via
//!   `.stream()` (`while let Some(chunk) = stream.next().await`)
//!   aborted past `MAX_RESULT_BYTES` as 502, under the same
//!   deadline — never a whole-body `.bytes()` collect.
//! - `fetch_output` races BOTH its Container fetch and the relaying
//!   R2 put against that one armed deadline; its output never passes
//!   through `.bytes()`.
//!
//! Out of scope: the result-byte cap values and the 500 ms
//! health-pacing sleep (all unchanged).
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

/// `fetch_output` runs to `leak_code`, the next definition.
fn fetch_output_code(code: &str) -> &str {
    fn_slice(code, "async fn fetch_output", "fn leak_code")
}

/// Collapse every run of whitespace (including newlines) to one
/// space, so a pin matches the arming expression whether rustfmt
/// keeps it on one line or breaks it across lines.
fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
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
    armed_before_token(slice, code, "fetch_request(")
}

/// Same arm-before-issue check for a site whose fetch is spelled
/// differently (`fetch_output` calls `.fetch(url, ...)`).
fn armed_before_token(slice: &str, code: &str, fetch_token: &str) -> bool {
    let Some(fetch_at) = slice.find(fetch_token) else {
        return false;
    };
    let mut arms: Vec<usize> = [DEADLINE_CONST, "Delay::from"]
        .iter()
        .filter_map(|t| slice.find(t))
        .collect();
    arms.extend(helper_call_positions(slice, code));
    arms.iter().any(|arm| *arm < fetch_at)
}

/// The streamed body read is inside the bound, not a bare await
/// after a headers-only race: a `.stream()`-fed read raced against
/// the deadline (a race token, or a shared-helper call doing the
/// racing, positioned after the stream read is constructed).
fn stream_body_covered(slice: &str, code: &str) -> bool {
    let Some(stream_at) = slice.find(".stream()") else {
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
    tokens.iter().any(|t| *t > stream_at)
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
        !slice.contains(".bytes()"),
        "stage_input disposes of the PUT reply by cancelling it, not by \
         collecting it with `.bytes()`"
    );
    assert!(
        slice.contains("cancel"),
        "stage_input must keep cancelling the unread PUT reply body"
    );
}

#[test]
fn container_json_still_caps_the_reply_bytes() {
    let code = code();
    let slice = container_json_code(&code);
    assert!(
        slice.contains("fetch_request(") && slice.contains(".stream()"),
        "container_json must keep exchanging through the port fetcher and reading the body as a stream"
    );
    assert!(
        slice.contains("> MAX_RESULT_BYTES"),
        "container_json must keep rejecting replies over MAX_RESULT_BYTES"
    );
    assert!(
        !slice.contains(".bytes()"),
        "container_json must stream the Container-chosen reply under the \
         cap, not collect it whole with `.bytes()` before rejecting it"
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
    // `stage_input` performs no body read at all: the fetch race
    // covers the upload plus the reply headers, and the unread reply
    // is cancelled. Any `.bytes()` here would sit outside that bound.
    let stage = stage_input_code(&code);
    assert!(
        !stage.contains(".bytes()"),
        "stage_input must perform no body read outside the deadline: the \
         PUT reply is cancelled, not collected"
    );
    assert!(
        stage.contains("cancel"),
        "stage_input must still dispose of the PUT reply by cancelling it"
    );
    assert!(
        stream_body_covered(container_json_code(&code), &code),
        "container_json's streamed `.stream()` read must be inside the deadline, \
         not a bare await after a headers-only race"
    );
}

// ---------------------------------------------------------------------------
// Scoped budgets: each pin below fails on one single-site revert of
// the per-operation semantics (repair path selection, size scaling,
// expiry-only drain, unit of the arming expression) that the flat
// races-a-timer pins above cannot see.
// ---------------------------------------------------------------------------

#[test]
fn each_site_arms_once_from_its_budget_in_millis() {
    // `Duration::from_millis(budget_ms)` exactly once per site: the
    // arming expression and its unit are the pin. `from_secs(...)`
    // would arm 1000x the budget while still naming the constants
    // and racing, so the race pins above cannot see it.
    let code = code();
    for (name, slice) in [
        ("stage_input", stage_input_code(&code)),
        ("container_json", container_json_code(&code)),
        ("fetch_output", fetch_output_code(&code)),
    ] {
        let flat = collapse_ws(slice);
        assert_eq!(
            flat.matches("Duration::from_millis(budget_ms)").count(),
            1,
            "{name} must arm its deadline exactly once as \
             `Duration::from_millis(budget_ms)`"
        );
        assert!(
            !flat.contains("Duration::from_secs("),
            "{name} must not arm with from_secs: budget_ms is already milliseconds"
        );
    }
}

#[test]
fn container_json_selects_the_repair_budget_by_path() {
    // `/repair` runs par2 under the Container's 120 s budget: only
    // the path selection keeps a legitimate repair from expiring at
    // the 10 s base. Stripping blanks string literals, so `/repair`
    // itself is invisible in `code()`; the selection is therefore
    // pinned on the raw source, and the constant order on `code`
    // (repair budget first, base as the else arm) is pinned here so
    // a swapped-arm rewrite still fails.
    let code = code();
    assert!(
        code.contains("const CONTAINER_REPAIR_DEADLINE_MS: u64 = 150_000;"),
        "runtime.rs must declare `const CONTAINER_REPAIR_DEADLINE_MS: u64 = 150_000;`"
    );
    let raw = fn_slice(
        RUNTIME_RS,
        "async fn container_json",
        "async fn read_object_capped",
    );
    assert!(
        collapse_ws(raw).contains("let budget_ms = if path == \"/repair\" {"),
        "container_json must derive budget_ms by selecting on `path == \"/repair\"`"
    );
    let slice = container_json_code(&code);
    let repair_at = slice
        .find("CONTAINER_REPAIR_DEADLINE_MS")
        .expect("container_json must reference CONTAINER_REPAIR_DEADLINE_MS");
    let base_at = slice
        .find(DEADLINE_CONST)
        .expect("container_json must keep CONTAINER_FETCH_DEADLINE_MS as the base arm");
    assert!(
        repair_at < base_at,
        "container_json must arm CONTAINER_REPAIR_DEADLINE_MS for /repair with \
         CONTAINER_FETCH_DEADLINE_MS as the other (else) arm, in that order"
    );
}

#[test]
fn stage_and_output_budgets_scale_with_transfer_size() {
    // The streamed upload/download rides inside the fetch, so a flat
    // 10 s expiry kills large healthy transfers mid-stream; the
    // budget must add `size/1024` ms atop the base.
    let code = code();
    let stage = collapse_ws(stage_input_code(&code));
    assert!(
        stage.contains(
            "let budget_ms = CONTAINER_FETCH_DEADLINE_MS.saturating_add(file.size.saturating_div(1024));"
        ),
        "stage_input must derive budget_ms as the 10 s base plus file.size/1024 ms"
    );
    let output = collapse_ws(fetch_output_code(&code));
    assert!(
        output.contains(
            "let budget_ms = CONTAINER_FETCH_DEADLINE_MS.saturating_add(output.size.saturating_div(1024));"
        ),
        "fetch_output must derive budget_ms as the 10 s base plus output.size/1024 ms"
    );
}

#[test]
fn stage_input_drain_transport_error_falls_through_to_status() {
    // The PUT reply is never read: it is cancelled with its result
    // ignored, so no drain transport error can override the verdict
    // — mapping a cancel failure to 502 (the drain-override shape)
    // must not come back. Only the fetch race itself (expiry or
    // transport failure) fails as 502; the known status below keeps
    // its mapping.
    let code = code();
    let slice = stage_input_code(&code);
    assert!(
        collapse_ws(slice).contains("let _ = stream.cancel();"),
        "stage_input must cancel the unread PUT reply with its result ignored, \
         via `let _ = stream.cancel();`"
    );
    assert!(
        !slice.contains(".bytes()"),
        "stage_input must not read the PUT reply it cancels"
    );
    assert!(
        !slice.contains("cancel().map_err") && !slice.contains("cancel()?"),
        "stage_input's cancel must not map a disposal failure to a fault: \
         the status match owns the verdict"
    );
    assert!(
        slice.contains("201 =>") && slice.contains("400 | 413"),
        "stage_input must keep mapping the known status after the drain"
    );
}

#[test]
fn fetch_output_races_fetch_and_put_against_the_deadline() {
    // `fetch_output` had no slice at all: its Container fetch and
    // the relaying R2 put must both race the one armed deadline,
    // and its streamed output must never be materialised via
    // `.bytes()`.
    let code = code();
    let slice = fetch_output_code(&code);
    assert!(
        site_races_deadline(slice, &code),
        "fetch_output must race its Container fetch against a Delay armed \
         from CONTAINER_FETCH_DEADLINE_MS"
    );
    assert!(
        armed_before_token(slice, &code, ".fetch(url,"),
        "fetch_output must arm the deadline before issuing its Container fetch"
    );
    assert!(
        slice.matches("poll_fn").count() >= 2,
        "fetch_output must race BOTH the fetch and the R2 put against the \
         deadline (one race each)"
    );
    let put_at = slice
        .find(".put(")
        .expect("fetch_output must relay the output into R2 with .put(");
    assert!(
        slice[put_at..].contains("poll_fn"),
        "fetch_output's R2 put must itself be raced against the deadline, \
         not awaited bare after a headers-only race"
    );
    assert!(
        !slice.contains(".bytes()"),
        "fetch_output streams its output; it must not materialise it via .bytes()"
    );
}
