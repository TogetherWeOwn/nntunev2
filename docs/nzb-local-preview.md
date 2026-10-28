# Offline local NZB claims preview

## Scope and current shipment hold

This page reads a selected file **only after Preview**. Selection itself checks
metadata only. Import remains unavailable. Results are aggregate claims, not
content availability, safety, ownership, playability or server intake acceptance.
No network, storage, telemetry or provider access is part of the preview.

**Not ready to ship:** a conservative pre-parser markup guard now rejects the
original near-2-MiB tiny-element reproducer without invoking the parser. However,
a parser-eligible element/comment-density case still exceeded the unchanged
1,000 ms whole-preview budget in two of five local Chromium repetitions. Firefox
and WebKit remain unverified on the local runner because system libraries are
absent; the existing CI Playwright container is the supported all-engine runner.
The independently merged 330 KiB repository bundle policy now applies explicitly;
no further budget increase, CSP change or new dependency is authorized.

## One implementation and offline checker

- Pure framing and aggregate logic: `apps/web/src/lib/library/nzb-preview.ts`.
- Built-in XML parser adapter and abort/generation lifecycle:
  `apps/web/src/routes/library/import/+page.svelte`.
- Stable constant messages: `apps/web/src/lib/library/nzb-preview-copy.ts`.
- Independent core/cap-drift bank:
  `apps/web/src/lib/library/nzb-preview.acceptance.test.ts`.
- Independent browser/copy/no-echo/no-network/no-storage bank:
  `apps/web/tests/nzb-preview.acceptance.spec.ts`.
- Additional queued-parser cancellation and fallback bounds:
  `apps/web/tests/nzb-preview-lifecycle.spec.ts`.
- Additional hostile near-2-MiB density/depth budgets:
  `apps/web/tests/nzb-preview-hostile-budget.spec.ts`.

Run from the repository root with the existing locked dependencies and provisioned
browser runtime. All inputs are generated synthetics, never real NZB files:

```sh
pnpm --dir apps/web check
pnpm --dir apps/web exec vitest run --maxWorkers=2
CI=1 PUBLIC_API_BASE_URL='' pnpm --dir apps/web exec playwright test tests/nzb-preview.acceptance.spec.ts tests/nzb-preview-lifecycle.spec.ts tests/nzb-preview-hostile-budget.spec.ts --workers=1
python3 .github/scripts/check_web_bundle_budget.py
```

The acceptance bank emits compact JSON records with stable `case_id`,
`expected`, `actual`, `status`, `error_code` and `next_action`. Timing cases also
record the engine and five `(parse_ms,total_ms)` repetitions. Failure has no
partial summary. The page's `__nzbpLastOutcome` inspection seam contains only the
same aggregate/error outcome, or cancellation; selection, Clear and teardown
clear it. It is not persisted or logged.

A browser engine is supported only after the checks actually run on it. Merely
installing the engine is not verification. No test runner's missing-library
validation may be bypassed. Synthetic checks are idempotent; there is no
operational CLI or production mode.

## Bounds and semantics

Intake-v1 mirrored caps are 2 MiB actual bytes, 5,000 file entries, 60,000 segments
and 64 GiB declared-byte claims. The drift assertion reads the server constants
and metadata validation cap. Additional local caps are depth 16, 150,000 elements,
16 attributes per element, 1,024 expanded UTF-16 code units per attribute value,
512 trimmed descendant-text units per segment and a 256-byte XML declaration
window. Root depth is one. There are no partial or truncated summaries.

The byte cap, fail-closed prolog framing and a conservative literal-markup bound
run before parsing. A linear, constant-memory scan counts every literal `<` in
the original decoded pre-excision text and rejects the 150,001st with
`NZBP_TOO_MANY_NODES`. Exactly 150,000 remains eligible, not guaranteed valid.
Closing tags, declarations, DTDs, comments, PIs and CDATA literals all count;
false rejection is deliberate, with no region-aware rescue. This is not an exact
element count or a changed intake limit. The separate 150,000-element traversal
cap remains. The built-in parser still materializes the entire eligible document
synchronously **before** depth, actual-element, attribute and text checks run;
passing the markup guard is not proof of the timing budgets.

The prolog grammar skips whole comments and processing instructions, accepts at
most one subset-free exact-case DOCTYPE, excises it and asserts that the exact
parser input contains no DOCTYPE token. Any entity-declaration literal anywhere
is rejected, including inside comments or CDATA. This deliberate false rejection
avoids a second, region-aware XML tokenizer. No external DTD reaches the parser.

The root must have local name `nzb` and either no namespace or the standard
newzbin namespace. Below the root, case-sensitive local-name `file` and `segment`
counts apply everywhere, regardless of prefix, parent path or namespace, matching
intake's cap-counting policy. A segment belongs to its nearest enclosing file
entry for warnings and duplicate number accounting; unparented segments still
count globally. Segment groups are counted within that entry. Only unprefixed
`bytes` and `number` attributes are interpreted; all attributes are length-checked
without retaining non-numeric values. Segment identity is concatenated descendant
text, trimmed; duplicate identifiers are transient and never returned.

All structural and string checks finish before numeric interpretation. Empty,
signed, padded, hex or otherwise non-ASCII-digit byte attributes mean an unknown
claim, not zero. Only explicit all-digit zero is a known zero. Values and sums use
BigInt under the 64 GiB bound. Segment numbers compare as canonical decimal
strings, never floating-point numbers. Number conversion is for bounded display
rounding only; the outcome retains the exact decimal sum.

String caps necessarily measure **after** predefined/numeric character reference
expansion, in JavaScript UTF-16 units. Such references consume more source than
the text they produce, and general/internal entities cannot survive framing, so
there is no entity amplification source. Numeric references expanding to digits
are valid preview byte values; intake's raw lexical scan differs. Build-time cap
parity is not server validation parity. A readable preview does not mean a future
import would accept this file.

## Lifetimes and cancellation

Every selection mutation invalidates the generation before changing state.
A preview also captures the File object and name/size/lastModified snapshot.
Replacement with identical metadata still invalidates by generation and object
identity. Stream cancellation calls `reader.cancel()` and releases its lock.
Actual chunk byte lengths are summed before copying or decoding. Retained chunks
are at most 64 KiB, with a task yield between them. The arrayBuffer fallback is
non-interruptible: its actual length is checked before decode and cancelled
results are generation-discarded.

After synchronous parsing returns, a task turn runs before either traversal or
error inspection/publication. Queued Cancel, Clear, replace and chooser handlers
therefore win. Errors also pass the final generation/snapshot gate. Parser error
text and exception messages are never read. Parsed nodes never enter the live
page; only constant text and numeric aggregates render. Unmount clears the probe
and invalidates in-flight work.

## Fixed timing gate and observed failure

Budgets were fixed before implementation: **500 ms parser / 1,000 ms whole local
preview**, every hostile case, five repetitions per claimed engine.

Measured on Linux x86_64 with Playwright 1.64.1, Chromium 156.0.8078.4, one worker:
`NZBP-R2-extra-tiny-elements`, 2,096,011 bytes, 524,000 tiny child elements.

| Repetition | Parser ms | Whole preview ms | Verdict |
| --- | ---: | ---: | --- |
| 1 | 569.10 | 1580 | FAIL both bounds |
| 2 | 892.00 | 1785 | FAIL both bounds |
| 3 | 457.50 | 1109 | FAIL whole-preview bound |
| 4 | 435.20 | 838 | PASS |
| 5 | 354.80 | 827 | PASS |

The less-dense independent bank's six cases passed five repetitions each, but
that does not supersede this failure. Near-cap comment/text density and deep
markup also passed five repetitions each. No file contents are included in these
receipts. The failure required security disposition and a new measured result
before shipment, not retries until a favorable timing appears. The approved
remediation adds only the conservative 150,000-literal-markup bound above.

## Guarded worktree remeasurement (not an exact-head pass)

The guarded original reproducer was rejected with zero parser invocations in all
five repetitions (197 ms maximum total). Parser durations are **not invoked / N/A**,
not a fabricated measured zero. Six pure guard cases pass, including the boundary,
comment/CDATA/PI literal false rejection, pre-excision counting and numeric
references not becoming markup syntax. The all-engine browser checker now covers
16 shapes, five individual repetitions each; the existing CI container runs it
without new host provisioning.

A guarded, parser-eligible 1,200,011-byte case (60,000 tiny elements and 40,000
comments) still failed the unchanged total budget on the local Chromium runner:

| Repetition | Parser ms | Whole preview ms | Verdict |
| --- | ---: | ---: | --- |
| 1 | 110.50 | 764 | PASS |
| 2 | 168.70 | 1275 | FAIL whole-preview bound |
| 3 | 115.40 | 772 | PASS |
| 4 | 115.60 | 647 | PASS |
| 5 | 82.30 | 1181 | FAIL whole-preview bound |

This was a pre-commit guarded worktree measurement based on the initial draft
head, not exact-head shipping proof. The remaining 15 Chromium shapes passed;
that does not discard these failures. No cap was lowered ad hoc and no cold run
was discarded. Shipment STOP remains for the new finding and the unverified
engines. Guarded rejections must stay under 1,000 ms without invoking the parser;
parser-eligible cases still require every parser step under 500 ms and every
whole preview under 1,000 ms on all three engines.

## Follow-on baseline and diagnostic contract

Shipment remains stopped. The permitted next step is an uninstrumented baseline
on the existing pinned Playwright container, followed by a separate committed
diagnostic head if phase instrumentation is needed. Browser/Playwright versions
and the one-worker configuration stay fixed across baseline and candidate; two
hosted jobs are not asserted to be the same physical machine. The application
sources remain byte-identical to `a6e613c23364839d2238619f9e60bf71535d5c51`
while preparing this baseline. Independently authored keyboard/read-lifecycle
acceptance corrections are integrated without altering their contents.

The first hosted run's 48 hostile-budget cases all aborted while reading Git
metadata because the container checkout has different ownership. These are
**incomplete measurements**, not 240 passing rows or evidence that the local
budget failures disappeared. The evidence helper now trusts only its exact
checkout for the two read-only Git calls, without writing global configuration.
Its regression uses Git's ownership-mismatch test mode; the original calls fail
and the scoped calls pass. Run it without installing any package:

```sh
node --test apps/web/tests/nzb-preview-evidence.node-test.mjs
```

No hostile input, expected outcome, parser spy, external click-to-outcome timer,
five-repetition/browser reuse rule or budget is changed. The complete browser
suite remains enabled; one worker is explicit, and the budget checker refuses a
multi-worker override. A branch-targeted manual CI run identifies the exact
baseline commit rather than treating a pull-request merge-ref build as the same
source. No same-code retry is permitted to search for favorable timings.

Before an optimization, diagnostics must locate material adapter/traversal cost
and attribute it to wrapper/iterator allocations with counts and aggregate
CPU/GC or equivalent evidence. Time outside the parser alone is insufficient.
Only then is one allocation-reduction approach to the adapter and necessary
temporary traversal bookkeeping permitted. All caps, global error precedence,
post-expansion string checks, BigInt arithmetic, read yields and cancellation
checks remain fixed. A different cause returns for security disposition rather
than triggering a speculative rewrite.

The eventual clean committed candidate requires all **240 individual rows**:
16 frozen shapes × five repetitions × Chromium/Firefox/WebKit, including first
and cold runs. Each row retains source/runner identity, actual bytes, expected
and actual outcome, parser count/duration (N/A when guarded), external total and
verdict. Any missing engine, crash, wrong result or over-budget repetition keeps
the STOP. Diagnostics never replace acceptance evidence or excuse a failed row.
