# Inert Subsonic snapshot construction accounting

The inert builder rejects all borrowed rows before allocation: database transfer
bounds do not prove allocation capacities or precharge retained rows, and even
an empty slice may have a retained backing allocation. No mount is enabled.

The Worker manifest pins Rust 1.98.1 and its wasm32 standard library. The
construction ledger computes a provisional charge for each nonempty allocation
using dlmalloc rounding, minimum, overhead and remainder allowance; unsupported
alignments and checked arithmetic overflow are rejected. Charges are cumulative
and never refunded. The formula is not yet a candidate-specific allocation bound:
the source and build coupling described below remain unverified. It makes no
claim about isolate RSS, allocator free-list retention or runtime and driver memory.

| Owner or operation | Required charge and lifetime |
| --- | --- |
| Raw artist, release and track rows | The supplier must precharge backing vector capacities and every owned string capacity, including optional and otherwise unused fields, before allocation. The rows overlap mapping and tracks remain retained afterwards. A length or SQL octet limit is not a receipt. |
| Output artist, album and song vectors | One charge per nonempty vector for its proved requested element capacity times the target type size and alignment, before allocation. All three overlap the raw rows. |
| Output strings | One charge per nonempty copy, including album artist sort names, both song release-ID copies, and the fixed creation date, content type and suffix. Copies are retained by the output models. |
| Lookups and ordering | Borrowed scans and stable in-place ordering avoid separately allocated maps and sorting scratch; preserve last-row-wins lookups and equal-key occurrence order. Ordering rejects a caller-supplied comparison/probe work limit before mutation; string comparison cost and planner scans are not covered, so a future caller must also bound input lengths and establish an acceptable CPU limit. |
| Reallocation | Avoid growth beyond precharged capacity. If a covered path reallocates, precharge the new block while retaining the old charge; cumulative accounting does not refund the old block. |

For the pinned standard library, `String::try_reserve_exact` delegates to its
byte vector, and `Vec::try_reserve_exact` passes its length to `RawVec`.
For an empty, non-zero-sized vector and a nonzero request, `needs_to_grow`
selects `grow_exact`, which checks `len + additional`. `finish_grow` computes
`layout_array` and sees no `current_memory` when capacity is zero, so it calls
`allocate`, not `grow`; `set_ptr_and_cap` records the requested capacity.
`Global::alloc_impl_runtime` returns a slice of the requested layout size.
A zero additional request on an empty vector does not grow; zero-sized element
vectors have different capacity semantics and are not covered by this argument.
`try_allocate_in` is a separate direct-allocation path, not the empty-vector
reservation path. Both use the checked array layout.

In the pinned source, `raw_vec::layout_array` uses `Layout::repeat_packed`,
which checks the element size multiplication and calls `from_size_alignment`;
that constructor rejects a size whose alignment-rounded value exceeds
`isize::MAX` (`core/src/alloc/layout.rs`). Existing backing memory takes
`finish_grow`'s `grow` branch and is outside the fresh-allocation argument,
even when a later capacity equals the requested capacity. These facts do not
cover arbitrary clones, collection growth or database decoding. A future
constructor must reject absent raw-row provenance, verify the covered fresh paths,
and ensure each subsequent write stays within the reserved capacity.

### Allocator source analysis (not an admission certificate)

The following analysis is of the locally installed Rust 1.98.1 source and its
vendored dlmalloc 0.2.13. Candidate build verification must still establish that
these sources and the linked std match the canonical manifest; the analysis
alone does not establish that binding or enable nonempty construction.

For a fresh nonzero allocation with alignment at most eight, wasm `System`
(`library/std/src/sys/alloc/wasm.rs`) calls dlmalloc's `malloc`; the wrapper
(`library/vendor/dlmalloc-0.2.13/src/lib.rs`) selects `malloc`, not `memalign`.
In `src/dlmalloc.rs`, `request2size` rounds `requested + 4` up to eight with
minimum 16 (`request2size`, lines 247–253). The small-bin exact/next-bin path
(lines 278–291) returns a chunk
of `nb` or `nb + 8`; a larger small bin is split to `nb`. Both tree-bin
paths split to `nb` or consume a remainder below the 16-byte minimum
(`tmalloc_small` and `tmalloc_large`); multiples of eight make that remainder
at most eight. The designated-victim path has the same split-or-whole rule.
The top path returns a chunk of `nb`. `sys_alloc` either splits its top
(`dlmalloc.rs` lines 445–457) or, when a newly supplied segment precedes an
existing one, calls `prepend_alloc` (lines 430–438); that function marks the
first chunk in use with exactly `size`, which is `nb` (lines 745–751). A failed
system allocation returns null. The wasm system allocator can first donate
linker-provided space or grow linear memory (`wasm.rs` lines 88–98); neither
route changes that chunk-size selection. Thus, subject to those implementation
invariants, `max(16, align_up(requested + 4, 8)) + 8`
bounds the allocated chunk, not allocator pages or retained free space.
Requests the allocator rejects do not produce a usable allocation. Reallocation
and alignments above eight are outside this argument. Source equality,
allocator-override refusal and build-to-artifact coupling remain necessary
before this becomes a candidate-specific proof. The current ledger test build
is not the ordinary Worker bundle: verifying a toolchain before a separate
build, or scanning only this repository for allocator declarations, does not
establish which allocator was linked into that bundle. A locked package list
alone also does not prove the absence of an allocator declaration in a resolved
dependency. The synthetic `ledger-wasm-test` artifact explicitly selects
`System`; Rust's duplicate-global-allocator diagnostic is intended to reject
another active Rust declaration in its linked graph. That guard is not in the
ordinary Worker build, does not cover direct allocator calls or custom arenas,
and a dependency-level negative build case still needs to pass in Actions.
Reliance on allocator symbol names in the final bundle requires proof that the
build preserves those names; stripped names cannot prove absence.
The existing build log checks do not verify the resulting WASM bytes. The current
compiler check trusts `rustc -vV` and `cargo -vV` output; it does not authenticate
the executable bytes or their host libraries. A substituted executable could
report the pinned version and still compile different code. Candidate provenance
must cover the actual compiler and build inputs as well as the output, under an
explicit trusted build environment. No artifact identity is claimed here.

Dispatch clones, response Value trees, rendering, database collection, and
platform buffers require their own accounting. This inventory alone does not
establish the complete 8 MiB graph or a concurrency guarantee. The AGENTS.md
Clean-room, YouTube and MetaBrainz guardrails remain applicable.
