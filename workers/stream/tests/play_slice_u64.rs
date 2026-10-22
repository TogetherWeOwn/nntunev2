//! Acceptance tests: staged-byte play slicing stays in `u64` until the
//! final in-bounds slice index.
//!
//! Do not edit these tests to make them pass: implement the contract in
//! production code. These files are owned by the test author; test
//! corrections stay with them.
//!
//! Contract under test (source `b8e8f744`, `workers/stream/src/play.rs`):
//!
//! - `:1374-1375` (cached serve) computes the chunk window as
//!   `(self.pos as usize + CHUNK).min(self.end as usize)` and slices
//!   `bytes[self.pos as usize..to]`;
//! - `:1383-1386` (live serve) computes `from`/`to` from
//!   `(self.pos - live.buf_start) as usize` and
//!   `(self.end - live.buf_start) as usize`, then slices `live.buf`;
//! - `:983` reports `self.staged_bytes as usize` into
//!   `YencError::SizeMismatch.actual` (today declared `usize` in
//!   `workers/stream/src/yenc.rs:20-23` next to a `u64` `expected`).
//!
//! `pos`, `end`, `buf_start` and `staged_bytes` are `u64` file offsets.
//! Narrowing them with `as usize` *before* the arithmetic/clamp truncates
//! on 32-bit targets (`wasm32`, the production Worker: `usize` is 32-bit):
//! a multi-GiB file offset wraps, so `end - buf_start` can collapse to a
//! small value (wrong/empty slice, or a slice-index panic) and a staged
//! byte total above 4 GiB is misreported. The fix keeps offset arithmetic
//! in `u64`, clamps against the in-memory buffer length in `u64`, and only
//! then converts the already-bounded local to the final `usize` slice
//! index (a clamped local cast or `usize::try_from` both pass); it also
//! widens `SizeMismatch.actual` so it carries the full staged total.
//!
//! Why source gates: CI hosts are 64-bit, where `as usize` is lossless and
//! every behavioural case below already passes, so no native behavioural
//! test can go red on the pre-fix code (a behavioural pin would need an
//! in-memory body over 4 GiB). The three source gates parse `play.rs` /
//! `yenc.rs` with `syn` and inspect token streams, so comments cannot
//! hide a cast and any host fails. They forbid only the narrowing shapes
//! named above — never cast `self.pos` / `self.end` / a `buf_start`
//! difference (under any alias or parenthesisation: the gate walks each
//! `as usize` cast's operand for `self.pos`, `self.end` or a `buf_start*`
//! identifier, recognises `usize` by its last path segment so qualified
//! spellings like `::std::primitive::usize` count, and propagates taint
//! through intermediate `let` locals — `let lim = self.end -
//! live.buf_start; lim as usize` narrows just as surely) /
//! `staged_bytes` directly, and never do arithmetic after a
//! narrowing cast (`as usize +`). Deliberately unpinned: the exact
//! widening syntax, local variable names, helper extraction, and the
//! chunk size.
//!
//! Sensitivity (what each pin kills; implementation stays untouched):
//!
//! - restoring any named narrowing in `Iterator for PlayStream` fails
//!   `play_slicing_keeps_offsets_in_u64_until_final_index`;
//! - restoring `staged_bytes as usize` anywhere in `play.rs` fails
//!   `staged_byte_count_is_not_narrowed_into_size_mismatch`;
//! - keeping `SizeMismatch.actual: usize` fails
//!   `size_mismatch_actual_field_is_u64`;
//! - breaking exact/off-by-one slicing (chunk, segment or range edges)
//!   fails the cached/live behavioural controls, which are green before
//!   and after the fix.
//!
//! Out of scope: sibling suites (untouched), checksum precedence, the
//! yEnc single-part size compares, and any range-parsing change.
//!
//! Run only this file:
//!
//! ```sh
//! cargo test --locked -p nntune-stream --test play_slice_u64
//! ```

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc,
};
use std::thread;
use std::time::Duration;

use nntune_stream::cache::ContentCache;
use nntune_stream::grant;
use nntune_stream::meter::MemoryMeter;
use nntune_stream::play::{play, PlayInput, PlayResponse, PlayStream, StreamPool};
use nntune_stream::pool::Pool;
use nntune_stream::yenc::{self, PartSpec, YencError};
use quote::ToTokens;

const SECRET: &[u8] = b"play-slice-u64-secret";
const NOW_UNIX: u64 = 1_700_000_000;
// Within the grant TTL cap (`MAX_GRANT_TTL` = 900 s).
const EXP_UNIX: u64 = NOW_UNIX + 900;
/// Larger than the 16 KiB serve chunk and a 20_000-byte segment, so both
/// boundaries fall inside the body.
const BODY_LEN: usize = 40_000;
const SEG_LEN: usize = 20_000;
/// Today's serve chunk (`play.rs` `CHUNK`): an adjacent-behaviour control,
/// not part of the u64 contract.
const CHUNK_CEILING: usize = 16 * 1024;

/// Play-path source, read as text and parsed (not imported): the RED
/// gates pin the narrowing textually via token streams so they fail on
/// 64-bit hosts too.
const PLAY_RS: &str = include_str!("../src/play.rs");
const YENC_RS: &str = include_str!("../src/yenc.rs");

/// Deterministic 40_000-byte plaintext.
fn body() -> Vec<u8> {
    (0..BODY_LEN).map(|i| (i % 251) as u8).collect()
}

// ---------------------------------------------------------------------------
// Source-text RED gates: fail on the pre-fix source on any host.
// ---------------------------------------------------------------------------

fn parse(src: &str, name: &str) -> syn::File {
    syn::parse_file(src).unwrap_or_else(|e| panic!("{name} must parse as Rust: {e}"))
}

/// The `impl Iterator for PlayStream` block (comments are not tokens, so
/// a commented-out cast cannot hide here and a cast named in a comment
/// cannot false-positive).
fn play_stream_iterator_impl(file: &syn::File) -> &syn::ItemImpl {
    for item in &file.items {
        let syn::Item::Impl(item_impl) = item else {
            continue;
        };
        let is_iterator = item_impl.trait_.as_ref().is_some_and(|(path, _)| {
            path.segments
                .last()
                .is_some_and(|seg| seg.ident == "Iterator")
        });
        let self_ty = item_impl.self_ty.to_token_stream().to_string();
        if is_iterator && self_ty.contains("PlayStream") {
            return item_impl;
        }
    }
    panic!("play.rs must still contain `impl Iterator for PlayStream`");
}

/// Token text of the `impl Iterator for PlayStream` block.
fn play_stream_iterator_tokens() -> String {
    let file = parse(PLAY_RS, "play.rs");
    play_stream_iterator_impl(&file)
        .to_token_stream()
        .to_string()
}

/// Which forbidden u64 offset a cast operand mentions, if any: `self.pos`,
/// `self.end`, or any identifier containing `buf_start` (so a renamed
/// alias such as `buf_start_local` or a field access `live.buf_start`
/// counts, under any parenthesisation).
fn narrowing_operand(expr: &syn::Expr) -> Option<&'static str> {
    let text = expr.to_token_stream().to_string();
    if text.contains("self . pos") {
        Some("`self.pos`")
    } else if text.contains("self . end") {
        Some("`self.end`")
    } else if text
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .any(|token| token.contains("buf_start"))
    {
        Some("a `buf_start*` identifier")
    } else {
        None
    }
}

/// True when `text` (a token-stream string) contains `name` as a whole
/// identifier token, not merely as a substring of a longer name.
fn contains_ident(text: &str, name: &str) -> bool {
    text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .any(|token| token == name)
}

/// A cast target is `usize` when the *last segment* of its path is
/// `usize`, so qualified spellings (`::std::primitive::usize`,
/// `core::primitive::usize`, `std::primitive::usize`) count too — an
/// exact-string compare against `"usize"` missed all of those.
fn is_usize_type(ty: &syn::Type) -> bool {
    match ty {
        syn::Type::Path(type_path) => type_path
            .path
            .segments
            .last()
            .is_some_and(|seg| seg.ident == "usize"),
        syn::Type::Paren(paren) => is_usize_type(&paren.elem),
        syn::Type::Group(group) => is_usize_type(&group.elem),
        _ => false,
    }
}

/// Unwrap parentheses/groups so shape checks see the inner expression.
fn unparen(mut expr: &syn::Expr) -> &syn::Expr {
    loop {
        match expr {
            syn::Expr::Paren(paren) => expr = &paren.expr,
            syn::Expr::Group(group) => expr = &group.expr,
            _ => return expr,
        }
    }
}

/// Whether `expr` is already bounded by an in-memory length (or a small
/// constant), so casting it to `usize` is the legitimate final-index
/// step rather than a truncation. `bounded` names locals already proven
/// bounded in this block.
/// - `.len()` and casts of it (`bytes.len() as u64`) are lengths;
/// - `a.min(b)` / `a.clamp(.., b)` is bounded when either side is;
/// - `a.max(b)` is bounded when both sides are;
/// - `len - x` is bounded by `len`;
/// - a bounded local or a `SCREAMING_CASE` constant (`CHUNK`) is.
fn expr_is_bounded(expr: &syn::Expr, bounded: &[String]) -> bool {
    use syn::Expr;
    match unparen(expr) {
        Expr::Lit(_) => true,
        Expr::Path(path) => path.path.segments.last().is_some_and(|seg| {
            let name = seg.ident.to_string();
            bounded.iter().any(|b| b == &name)
                || (name.len() > 1
                    && name
                        .chars()
                        .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_'))
        }),
        Expr::Cast(cast) => expr_is_bounded(&cast.expr, bounded),
        Expr::MethodCall(call) => {
            let method = call.method.to_string();
            match method.as_str() {
                "len" => true,
                "min" | "clamp" => {
                    expr_is_bounded(&call.receiver, bounded)
                        || call.args.iter().any(|a| expr_is_bounded(a, bounded))
                }
                "max" => {
                    expr_is_bounded(&call.receiver, bounded)
                        && call.args.iter().all(|a| expr_is_bounded(a, bounded))
                }
                "saturating_sub" | "min_by" => expr_is_bounded(&call.receiver, bounded),
                _ => false,
            }
        }
        Expr::Binary(binary) => match binary.op {
            syn::BinOp::Sub(_) => expr_is_bounded(&binary.left, bounded),
            syn::BinOp::Add(_) => {
                expr_is_bounded(&binary.left, bounded) && expr_is_bounded(&binary.right, bounded)
            }
            _ => false,
        },
        _ => false,
    }
}

/// Identifiers bound by a `let` pattern (simple, typed, tuple/or/ref).
fn pat_idents(pat: &syn::Pat, names: &mut Vec<String>) {
    match pat {
        syn::Pat::Ident(ident) => names.push(ident.ident.to_string()),
        syn::Pat::Type(typed) => pat_idents(&typed.pat, names),
        syn::Pat::Tuple(tuple) => {
            for elem in &tuple.elems {
                pat_idents(elem, names);
            }
        }
        syn::Pat::TupleStruct(tuple) => {
            for elem in &tuple.elems {
                pat_idents(elem, names);
            }
        }
        syn::Pat::Or(or) => {
            for case in &or.cases {
                pat_idents(case, names);
            }
        }
        syn::Pat::Reference(reference) => pat_idents(&reference.pat, names),
        syn::Pat::Slice(slice) => {
            for elem in &slice.elems {
                pat_idents(elem, names);
            }
        }
        syn::Pat::Struct(strct) => {
            for field in &strct.fields {
                pat_idents(&field.pat, names);
            }
        }
        _ => {}
    }
}

/// Taint state threaded through a block, in statement order.
/// - `tainted`: locals derived from an unbounded u64 play offset
///   (`self.pos`/`self.end`/`buf_start*`, or another tainted local)
///   whose value has not yet been bounded by a length clamp. Casting
///   one to `usize` is the truncation this gate forbids — even after
///   the offset is laundered through an intermediate name such as
///   `let lim = self.end - live.buf_start; lim as usize`.
/// - `bounded`: locals proven `<=` a buffer length (or chunk constant)
///   by `expr_is_bounded`, e.g. `start_u64 = self.pos.min(buf_len)`.
/// - `avail_of`: `(avail, offset)` pairs from `let avail = buf_len -
///   offset;`. The clean live serve then bounds its take with
///   `.min(avail)`; once that min is seen, `offset` is the established
///   in-buffer index and moves from tainted to bounded. A mutant that
///   casts the offset (or an end-derived `lim`) before any such clamp
///   never earns that move.
#[derive(Clone, Default)]
struct GateState {
    tainted: Vec<String>,
    bounded: Vec<String>,
    avail_of: Vec<(String, String)>,
}

impl GateState {
    fn taint_of(&self, expr: &syn::Expr) -> Option<String> {
        let text = expr.to_token_stream().to_string();
        self.tainted
            .iter()
            .find(|name| contains_ident(&text, name))
            .cloned()
    }

    /// Record `names = init` after init's own casts have been checked:
    /// a bounded init clears taint, a taint-sourced unbounded init
    /// propagates it, and a plain shadowing removes stale state.
    fn record_binding(&mut self, names: &[String], init: &syn::Expr) {
        let text = init.to_token_stream().to_string();
        // `let avail = <bounded length> - <tainted offset>;`
        if let syn::Expr::Binary(binary) = unparen(init) {
            if matches!(binary.op, syn::BinOp::Sub(_))
                && expr_is_bounded(&binary.left, &self.bounded)
            {
                if let Some(offset) = self.taint_of(&binary.right) {
                    for name in names {
                        self.avail_of.push((name.clone(), offset.clone()));
                    }
                }
            }
        }
        // Once an avail local is consumed by a `.min(...)` clamp, the
        // offset it was derived from is bounded by construction.
        if text.contains("min (") {
            let pairs = self.avail_of.clone();
            for (avail, offset) in pairs {
                if contains_ident(&text, &avail) {
                    self.tainted.retain(|t| t != &offset);
                    if !self.bounded.iter().any(|b| b == &offset) {
                        self.bounded.push(offset);
                    }
                }
            }
        }
        let bounded_init = expr_is_bounded(init, &self.bounded);
        let tainted_init = narrowing_operand(init).is_some() || self.taint_of(init).is_some();
        for name in names {
            self.tainted.retain(|t| t != name);
            self.bounded.retain(|b| b != name);
            self.avail_of.retain(|(a, _)| a != name || bounded_init);
            if bounded_init {
                self.bounded.push(name.clone());
            } else if tainted_init {
                self.tainted.push(name.clone());
            }
        }
    }
}

/// `expr as usize` violations inside one expression: a cast whose operand
/// token subtree still mentions an unbounded u64 offset, or a local
/// tainted by one (see `GateState`). A cast target counts as `usize` by
/// its last path segment. Hand-rolled `syn::Expr` recursion (this crate's
/// `syn` dev-dependency does not enable the `visit` feature, and only this
/// test file may change). Casts inside macro-invocation tokens are not
/// parsed as `Expr` and so are out of reach here; the literal token
/// patterns in the gate below still cover their direct spellings.
fn collect_narrowing_casts(expr: &syn::Expr, violations: &mut Vec<String>, state: &mut GateState) {
    use syn::Expr;
    if let Expr::Cast(cast) = expr {
        if is_usize_type(&cast.ty) {
            if let Some(what) = narrowing_operand(&cast.expr) {
                violations.push(format!(
                    "`{} as usize` (narrows {what} before the buffer-length clamp)",
                    cast.expr.to_token_stream()
                ));
            } else if !expr_is_bounded(&cast.expr, &state.bounded) {
                if let Some(name) = state.taint_of(&cast.expr) {
                    violations.push(format!(
                        "`{} as usize` (narrows tainted local `{name}` derived \
                         from an unbounded u64 play offset before the \
                         buffer-length clamp)",
                        cast.expr.to_token_stream()
                    ));
                }
            }
        }
    }
    match expr {
        Expr::Array(array) => {
            for elem in &array.elems {
                collect_narrowing_casts(elem, violations, state);
            }
        }
        Expr::Assign(assign) => {
            collect_narrowing_casts(&assign.left, violations, state);
            collect_narrowing_casts(&assign.right, violations, state);
            if let Expr::Path(path) = unparen(&assign.left) {
                if let Some(seg) = path.path.segments.last() {
                    if path.path.segments.len() == 1 {
                        let name = seg.ident.to_string();
                        state.record_binding(&[name], &assign.right);
                    }
                }
            }
        }
        Expr::Async(expr) => collect_narrowing_casts_in_block(&expr.block, violations, state),
        Expr::Await(expr) => collect_narrowing_casts(&expr.base, violations, state),
        Expr::Binary(expr) => {
            collect_narrowing_casts(&expr.left, violations, state);
            collect_narrowing_casts(&expr.right, violations, state);
        }
        Expr::Block(expr) => collect_narrowing_casts_in_block(&expr.block, violations, state),
        Expr::Break(expr) => {
            if let Some(inner) = &expr.expr {
                collect_narrowing_casts(inner, violations, state);
            }
        }
        Expr::Call(call) => {
            collect_narrowing_casts(&call.func, violations, state);
            for arg in &call.args {
                collect_narrowing_casts(arg, violations, state);
            }
        }
        Expr::Cast(cast) => collect_narrowing_casts(&cast.expr, violations, state),
        Expr::Closure(closure) => {
            let saved = state.clone();
            collect_narrowing_casts(&closure.body, violations, state);
            *state = saved;
        }
        Expr::Const(expr) => collect_narrowing_casts_in_block(&expr.block, violations, state),
        Expr::Field(expr) => collect_narrowing_casts(&expr.base, violations, state),
        Expr::ForLoop(expr) => {
            collect_narrowing_casts(&expr.expr, violations, state);
            collect_narrowing_casts_in_block(&expr.body, violations, state);
        }
        Expr::Group(group) => collect_narrowing_casts(&group.expr, violations, state),
        Expr::If(expr) => {
            collect_narrowing_casts(&expr.cond, violations, state);
            collect_narrowing_casts_in_block(&expr.then_branch, violations, state);
            if let Some((_, else_branch)) = &expr.else_branch {
                collect_narrowing_casts(else_branch, violations, state);
            }
        }
        Expr::Index(expr) => {
            collect_narrowing_casts(&expr.expr, violations, state);
            collect_narrowing_casts(&expr.index, violations, state);
        }
        Expr::Let(expr) => collect_narrowing_casts(&expr.expr, violations, state),
        Expr::Loop(expr) => collect_narrowing_casts_in_block(&expr.body, violations, state),
        Expr::Match(expr) => {
            collect_narrowing_casts(&expr.expr, violations, state);
            for arm in &expr.arms {
                let saved = state.clone();
                collect_narrowing_casts_in_pat(&arm.pat, violations, state);
                collect_narrowing_casts(&arm.body, violations, state);
                *state = saved;
            }
        }
        Expr::MethodCall(call) => {
            collect_narrowing_casts(&call.receiver, violations, state);
            for arg in &call.args {
                collect_narrowing_casts(arg, violations, state);
            }
        }
        Expr::Paren(expr) => collect_narrowing_casts(&expr.expr, violations, state),
        Expr::Range(range) => {
            if let Some(start) = &range.start {
                collect_narrowing_casts(start, violations, state);
            }
            if let Some(end) = &range.end {
                collect_narrowing_casts(end, violations, state);
            }
        }
        Expr::RawAddr(expr) => collect_narrowing_casts(&expr.expr, violations, state),
        Expr::Reference(expr) => collect_narrowing_casts(&expr.expr, violations, state),
        Expr::Repeat(expr) => {
            collect_narrowing_casts(&expr.expr, violations, state);
            collect_narrowing_casts(&expr.len, violations, state);
        }
        Expr::Return(expr) => {
            if let Some(inner) = &expr.expr {
                collect_narrowing_casts(inner, violations, state);
            }
        }
        Expr::Struct(expr) => {
            for field in &expr.fields {
                collect_narrowing_casts(&field.expr, violations, state);
            }
            if let Some(rest) = &expr.rest {
                collect_narrowing_casts(rest, violations, state);
            }
        }
        Expr::Try(expr) => collect_narrowing_casts(&expr.expr, violations, state),
        Expr::TryBlock(expr) => collect_narrowing_casts_in_block(&expr.block, violations, state),
        Expr::Tuple(expr) => {
            for elem in &expr.elems {
                collect_narrowing_casts(elem, violations, state);
            }
        }
        Expr::Unary(expr) => collect_narrowing_casts(&expr.expr, violations, state),
        Expr::Unsafe(expr) => collect_narrowing_casts_in_block(&expr.block, violations, state),
        Expr::While(expr) => {
            collect_narrowing_casts(&expr.cond, violations, state);
            collect_narrowing_casts_in_block(&expr.body, violations, state);
        }
        Expr::Yield(expr) => {
            if let Some(inner) = &expr.expr {
                collect_narrowing_casts(inner, violations, state);
            }
        }
        Expr::Continue(_)
        | Expr::Infer(_)
        | Expr::Lit(_)
        | Expr::Macro(_)
        | Expr::Path(_)
        | Expr::Verbatim(_)
        | _ => {}
    }
}

/// Match-arm guards are `Pat::Guard` in this `syn` version; a cast in a
/// guard is an expression too.
fn collect_narrowing_casts_in_pat(
    pat: &syn::Pat,
    violations: &mut Vec<String>,
    state: &mut GateState,
) {
    match pat {
        syn::Pat::Guard(guard) => {
            collect_narrowing_casts_in_pat(&guard.pat, violations, state);
            collect_narrowing_casts(&guard.guard, violations, state);
        }
        syn::Pat::Or(or) => {
            for case in &or.cases {
                collect_narrowing_casts_in_pat(case, violations, state);
            }
        }
        _ => {}
    }
}

fn collect_narrowing_casts_in_block(
    block: &syn::Block,
    violations: &mut Vec<String>,
    state: &mut GateState,
) {
    // Locals bound inside this block do not escape it: restore the
    // caller's taint/bounded sets on exit, but thread them statement
    // by statement so `let` taint propagates to later casts here.
    let saved = state.clone();
    for stmt in &block.stmts {
        match stmt {
            syn::Stmt::Local(local) => {
                if let Some(init) = &local.init {
                    collect_narrowing_casts(&init.expr, violations, state);
                    if let Some((_, diverge)) = &init.diverge {
                        collect_narrowing_casts(diverge, violations, state);
                    }
                    let mut names = Vec::new();
                    pat_idents(&local.pat, &mut names);
                    state.record_binding(&names, &init.expr);
                }
            }
            syn::Stmt::Expr(expr, _) => collect_narrowing_casts(expr, violations, state),
            syn::Stmt::Item(syn::Item::Fn(item_fn)) => {
                let mut inner = GateState::default();
                collect_narrowing_casts_in_block(&item_fn.block, violations, &mut inner)
            }
            syn::Stmt::Item(_) | syn::Stmt::Macro(_) => {}
        }
    }
    *state = saved;
}

/// Every `as usize` narrowing of a u64 play offset in
/// `impl Iterator for PlayStream`, whatever the local alias or
/// parenthesisation: this is what kills an aliased restore of the old
/// truncating live window on a 64-bit host.
fn play_stream_narrowing_casts() -> Vec<String> {
    let file = parse(PLAY_RS, "play.rs");
    let item_impl = play_stream_iterator_impl(&file);
    let mut violations = Vec::new();
    for item in &item_impl.items {
        if let syn::ImplItem::Fn(method) = item {
            let mut state = GateState::default();
            collect_narrowing_casts_in_block(&method.block, &mut violations, &mut state);
        }
    }
    violations
}

#[test]
fn play_slicing_keeps_offsets_in_u64_until_final_index() {
    let tokens = play_stream_iterator_tokens();
    // Token-stream text is single-spaced: `self . pos as usize`. The
    // `buf_start` difference is not pinned literally: renaming it to an
    // alias (`buf_start_local`) would slip a literal pattern yet still
    // truncate on wasm32, so `play_stream_narrowing_casts` below walks
    // every `ExprCast` to `usize` instead.
    let forbidden: [(&str, &str); 3] = [
        (
            "self . pos as usize",
            "the cached window/slice narrows the u64 `pos` before clamping",
        ),
        (
            "self . end as usize",
            "the cached window narrows the u64 `end` before clamping",
        ),
        (
            "as usize +",
            "arithmetic runs after a narrowing cast instead of in u64",
        ),
    ];
    let mut violations: Vec<String> = forbidden
        .iter()
        .filter(|(pattern, _)| tokens.contains(pattern))
        .map(|(pattern, why)| format!("`{pattern}` ({why})"))
        .collect();
    violations.extend(play_stream_narrowing_casts());
    assert!(
        violations.is_empty(),
        "`impl Iterator for PlayStream` in workers/stream/src/play.rs still \
         narrows u64 play offsets to usize before slicing: {}. Keep offset \
         arithmetic in u64, clamp against the buffer length in u64, and \
         cast only the already-bounded local to the final slice index.",
        violations.join("; ")
    );
    // The serve paths must still exist (deleting them would otherwise
    // pass the negative pin and fail only behaviourally below).
    assert!(
        tokens.contains("Cached") && tokens.contains("Live"),
        "PlayStream::next must still serve both the cached and live sources"
    );
}

#[test]
fn staged_byte_count_is_not_narrowed_into_size_mismatch() {
    let tokens = parse(PLAY_RS, "play.rs").to_token_stream().to_string();
    assert!(
        !tokens.contains("staged_bytes as usize"),
        "workers/stream/src/play.rs still narrows `staged_bytes` with \
         `as usize`; report the full u64 staged total in \
         `SizeMismatch.actual`"
    );
    assert!(
        tokens.contains("staged_bytes") && tokens.contains("SizeMismatch"),
        "play.rs must still track the staged total and report a size \
         mismatch when it disagrees with the declared file size"
    );
}

#[test]
fn size_mismatch_actual_field_is_u64() {
    let file = parse(YENC_RS, "yenc.rs");
    let mut fields: Vec<(String, String)> = Vec::new();
    for item in &file.items {
        let syn::Item::Enum(item_enum) = item else {
            continue;
        };
        if item_enum.ident != "YencError" {
            continue;
        }
        for variant in &item_enum.variants {
            if variant.ident != "SizeMismatch" {
                continue;
            }
            if let syn::Fields::Named(named) = &variant.fields {
                for field in &named.named {
                    if let Some(ident) = &field.ident {
                        fields.push((ident.to_string(), field.ty.to_token_stream().to_string()));
                    }
                }
            }
        }
    }
    assert!(
        !fields.is_empty(),
        "yenc.rs must still declare `YencError::SizeMismatch` with named \
         `expected`/`actual` fields"
    );
    for (name, ty) in &fields {
        assert_eq!(
            ty, "u64",
            "SizeMismatch.{name} must be u64 so it carries the full value \
             on 32-bit targets (found `{ty}`)"
        );
    }
}

// ---------------------------------------------------------------------------
// Behavioural controls: green before and after the fix. Exact and
// off-by-one slicing at chunk, segment and body edges, on both the
// cached (`Source::Cached`) and live/staged (`Source::Live`) paths.
// ---------------------------------------------------------------------------

struct CachedRig {
    pool: StreamPool,
    cache: ContentCache,
    meter: MemoryMeter,
}

impl CachedRig {
    /// Cache already holds `plain` for `track`: plays are hits and never
    /// dial the dummy server address.
    fn seeded(track: &str, plain: Vec<u8>) -> Self {
        let mut cache = ContentCache::new();
        cache.put_track(track, plain);
        Self {
            pool: Pool::new(1),
            cache,
            meter: MemoryMeter::default(),
        }
    }

    fn play_range(
        &mut self,
        track: &str,
        range: Option<&str>,
    ) -> Result<PlayResponse, nntune_stream::play::PlayError> {
        let token = grant::issue("u1", track, EXP_UNIX, SECRET);
        let addr: SocketAddr = "127.0.0.1:119".parse().expect("dummy addr");
        let input = PlayInput {
            trace_id: None,
            token: &token,
            track_id: track,
            range,
            server_addr: addr,
            segments: &[],
            segment_ranges: None,
            cipher_key: 0,
            now_unix: NOW_UNIX,
            up_next: None,
        };
        play(
            &mut self.pool,
            &mut self.cache,
            &mut self.meter,
            SECRET,
            &input,
        )
    }

    /// Drain while checking the chunking invariants every slice must keep:
    /// no empty chunks (a truncated window computing `to <= from` would
    /// stall or panic) and no chunk past the serve-chunk ceiling.
    fn drain(&mut self, mut stream: PlayStream) -> Vec<u8> {
        let mut out = Vec::new();
        for chunk in stream.by_ref() {
            assert!(!chunk.is_empty(), "a served chunk must never be empty");
            assert!(
                chunk.len() <= CHUNK_CEILING,
                "chunk of {} bytes exceeds the serve-chunk ceiling",
                chunk.len()
            );
            out.extend_from_slice(&chunk);
        }
        stream
            .finalize(&mut self.pool, &mut self.cache, &mut self.meter)
            .expect("finalize play");
        out
    }
}

#[test]
fn cached_full_play_slices_every_chunk_in_order() {
    let plain = body();
    let mut rig = CachedRig::seeded("track-full", plain.clone());
    let resp = rig
        .play_range("track-full", None)
        .expect("full play serves");
    assert_eq!(resp.status, 200);
    assert_eq!(resp.body_len, BODY_LEN as u64);
    assert!(resp.cache_hit);
    let served = rig.drain(resp.stream);
    assert_eq!(served, plain, "every byte served exactly once, in order");
}

#[test]
fn cached_range_crossing_chunk_boundary_serves_exact_bytes() {
    let plain = body();
    let mut rig = CachedRig::seeded("track-chunk", plain.clone());
    // 16_380..=16_390 straddles the first 16 KiB chunk boundary (16_384).
    let resp = rig
        .play_range("track-chunk", Some("bytes=16380-16390"))
        .expect("chunk-crossing range serves");
    assert_eq!(resp.status, 206);
    assert_eq!(
        resp.content_range.as_deref(),
        Some(format!("bytes 16380-16390/{BODY_LEN}").as_str())
    );
    assert_eq!(resp.body_len, 11);
    let served = rig.drain(resp.stream);
    assert_eq!(served, &plain[16_380..16_391]);
}

#[test]
fn cached_single_byte_ranges_at_body_edges() {
    let plain = body();
    let mut rig = CachedRig::seeded("track-edges", plain.clone());

    let resp = rig
        .play_range("track-edges", Some("bytes=0-0"))
        .expect("first-byte range serves");
    assert_eq!(resp.body_len, 1);
    assert_eq!(rig.drain(resp.stream), &plain[0..1]);

    let last = BODY_LEN - 1;
    let header = format!("bytes={last}-");
    let resp = rig
        .play_range("track-edges", Some(&header))
        .expect("last-byte open range serves");
    assert_eq!(resp.body_len, 1);
    assert_eq!(rig.drain(resp.stream), &plain[last..]);
}

/// Minimal NNTP stub serving articles by message-id (mirrors
/// `covering_fallback_pins.rs`), so covering/backfill fetch order on the
/// live path does not matter.
struct Server {
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    bodies: Arc<AtomicUsize>,
    handle: Option<thread::JoinHandle<()>>,
}

impl Server {
    fn start(articles: HashMap<String, Vec<u8>>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fake NNTP");
        let addr = listener.local_addr().expect("local addr");
        listener.set_nonblocking(true).expect("nonblocking");
        let stop = Arc::new(AtomicBool::new(false));
        let bodies = Arc::new(AtomicUsize::new(0));
        let (stop_c, bodies_c) = (Arc::clone(&stop), Arc::clone(&bodies));
        let handle = thread::spawn(move || {
            while !stop_c.load(Ordering::SeqCst) {
                let (stream, _) = match listener.accept() {
                    Ok(pair) => pair,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(_) => break,
                };
                Self::serve(stream, &articles, &bodies_c);
            }
        });
        Self {
            addr,
            stop,
            bodies,
            handle: Some(handle),
        }
    }

    fn serve(stream: TcpStream, articles: &HashMap<String, Vec<u8>>, bodies: &AtomicUsize) {
        let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
        let mut reader = BufReader::new(stream.try_clone().expect("clone conn"));
        let mut writer = stream;
        let _ = writer.write_all(b"200 synthetic NNTP ready\r\n");
        let mut line = Vec::new();
        loop {
            line.clear();
            match reader.read_until(b'\n', &mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
            while line.last() == Some(&b'\n') || line.last() == Some(&b'\r') {
                line.pop();
            }
            let cmd = String::from_utf8_lossy(&line).into_owned();
            if let Some(mid) = cmd
                .strip_prefix("BODY ")
                .map(|s| s.trim().trim_matches(|c| c == '<' || c == '>'))
            {
                bodies.fetch_add(1, Ordering::SeqCst);
                let full_mid = format!("<{mid}>");
                match articles.get(mid) {
                    Some(article) => {
                        let _ =
                            writer.write_all(format!("222 0 {full_mid} follows\r\n").as_bytes());
                        for raw_line in article.split(|&b| b == b'\n') {
                            let text_line = raw_line.strip_suffix(b"\r").unwrap_or(raw_line);
                            if text_line.first() == Some(&b'.') {
                                let _ = writer.write_all(b".");
                            }
                            let _ = writer.write_all(text_line);
                            let _ = writer.write_all(b"\r\n");
                        }
                        let _ = writer.write_all(b".\r\n");
                    }
                    None => {
                        let _ = writer.write_all(b"430 no such article\r\n");
                    }
                }
            } else if cmd == "QUIT" {
                let _ = writer.write_all(b"205 bye\r\n");
                break;
            } else {
                let _ = writer.write_all(b"500 unknown command\r\n");
            }
        }
    }

    fn bodies(&self) -> usize {
        self.bodies.load(Ordering::SeqCst)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.addr);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// `(plaintext, segment message-ids, segment ranges, stub server)`.
type LiveFixture = (Vec<u8>, Vec<String>, [(u64, u64); 2], Server);

/// Two-segment live fixture over `body()`; identity cipher (key 0).
fn live_fixture() -> LiveFixture {
    let plain = body();
    let crc = yenc::crc32(&plain);
    let total = plain.len().div_ceil(SEG_LEN) as u32;
    let mut articles = HashMap::new();
    let mut segments = Vec::new();
    for (i, chunk) in plain.chunks(SEG_LEN).enumerate() {
        let part = i as u32 + 1;
        let mid = format!("slice-u64-seg{i}@synthetic.test");
        articles.insert(
            mid.clone(),
            yenc::encode_part(
                chunk,
                "slice-u64.flac",
                PartSpec {
                    file_size: plain.len() as u64,
                    part,
                    total,
                    offset: (i * SEG_LEN) as u64,
                    file_crc: (part == total).then_some(crc),
                },
            ),
        );
        segments.push(mid);
    }
    let ranges = [(0u64, SEG_LEN as u64), (SEG_LEN as u64, BODY_LEN as u64)];
    (plain, segments, ranges, Server::start(articles))
}

fn live_play(
    server: &Server,
    segments: &[String],
    ranges: &[(u64, u64)],
    range: Option<&str>,
) -> (PlayResponse, CachedRig) {
    let mut rig = CachedRig {
        pool: Pool::new(4),
        cache: ContentCache::new(),
        meter: MemoryMeter::default(),
    };
    let token = grant::issue("user-1", "track-live", EXP_UNIX, SECRET);
    let input = PlayInput {
        trace_id: None,
        token: &token,
        track_id: "track-live",
        range,
        server_addr: server.addr,
        segments,
        segment_ranges: Some(ranges),
        cipher_key: 0,
        now_unix: NOW_UNIX,
        up_next: None,
    };
    let resp = play(
        &mut rig.pool,
        &mut rig.cache,
        &mut rig.meter,
        SECRET,
        &input,
    )
    .expect("live play must serve");
    (resp, rig)
}

#[test]
fn live_full_play_slices_across_segment_boundary() {
    let (plain, segments, ranges, server) = live_fixture();
    let (resp, mut rig) = live_play(&server, &segments, &ranges, None);
    assert_eq!(resp.status, 200);
    assert!(!resp.cache_hit);
    assert_eq!(resp.body_len, BODY_LEN as u64);
    // Serving crosses into segment 1, whose staged buffer starts at
    // `buf_start = SEG_LEN`: `pos - buf_start` must stay exact there.
    let served = rig.drain(resp.stream);
    assert_eq!(served, plain);
    assert_eq!(
        rig.cache
            .get_track("track-live")
            .map(|b| b.to_vec())
            .unwrap_or_default(),
        plain,
        "finalize stages the whole track into the cache"
    );
    assert!(server.bodies() >= 2, "both segments fetched");
}

#[test]
fn live_range_crossing_segment_boundary_serves_exact_bytes() {
    let (plain, segments, ranges, server) = live_fixture();
    // 19_995..=20_004 straddles the segment boundary at 20_000.
    let (resp, mut rig) = live_play(&server, &segments, &ranges, Some("bytes=19995-20004"));
    assert_eq!(resp.status, 206);
    assert_eq!(
        resp.content_range.as_deref(),
        Some(format!("bytes 19995-20004/{BODY_LEN}").as_str())
    );
    assert_eq!(resp.body_len, 10);
    let served = rig.drain(resp.stream);
    assert_eq!(served, &plain[19_995..20_005]);
    assert_eq!(
        rig.cache
            .get_track("track-live")
            .map(|b| b.to_vec())
            .unwrap_or_default(),
        plain,
        "finalize backfills and stages the whole track"
    );
}

#[test]
fn live_range_inside_second_segment_serves_exact_bytes() {
    let (plain, segments, ranges, server) = live_fixture();
    // Entirely inside segment 1 (`buf_start = 20_000`): the slice offset
    // is the in-buffer difference, not the absolute file offset.
    let (resp, mut rig) = live_play(&server, &segments, &ranges, Some("bytes=35000-35099"));
    assert_eq!(resp.status, 206);
    assert_eq!(resp.body_len, 100);
    let served = rig.drain(resp.stream);
    assert_eq!(served, &plain[35_000..35_100]);
}

// ---------------------------------------------------------------------------
// SizeMismatch reporting control: exact small values before and after the
// field widening (integer literals infer to either field type).
// ---------------------------------------------------------------------------

#[test]
fn size_mismatch_reports_exact_small_actual() {
    // `=ybegin` declares 4 bytes, the body decodes to exactly 3.
    let text = b"=ybegin line=128 size=4 name=a.bin\r\nXYZ\r\n=yend size=3\r\n";
    assert!(
        matches!(
            yenc::decode_part(text),
            Err(YencError::SizeMismatch {
                expected: 4,
                actual: 3
            })
        ),
        "a one-byte size lie must still report the exact actual byte count"
    );
}
