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
In `src/dlmalloc.rs`, wasm32 gives a four-byte `usize` and pointer, so the
four-field `Chunk` is 16 bytes (`Chunk`, lines 61–67), `malloc_alignment` is
8 and `chunk_overhead` is 4 (lines 145–152). `min_chunk_size` aligns the
chunk size to eight (lines 174–176), and `pad_request` aligns the request plus
the overhead (lines 202–204). Thus `request2size` selects
`max(16, align_up(requested + 4, 8))` (lines 179–181, 247–253). Small-bin
indices encode multiples of eight (lines 50, 206–212), and the tree and
designated-victim split paths subtract the aligned request from a free chunk.
The resulting remainder is a multiple of eight only if that chunk is aligned;
this relies on
the allocator's chunk invariants, including those maintained by free and
coalescing; a debug assertion by itself is not an independent proof. In
particular, the linker-provided initial heap path (`wasm.rs` lines 17–31,
88–96) donates the difference between linker symbols without rounding the
length, while `init_top` (`dlmalloc.rs` lines 725–735) does not round its
size. For the initial segment, `sys_alloc` subtracts `top_foot_size` before
`init_top`; that footprint is a multiple of eight. `init_top` aligns the chunk's
user pointer, which is eight bytes past the chunk address on wasm32, so its
resulting top address is also eight-aligned. Its end is `__heap_end` minus the
footprint. Consequently, eight-byte alignment of `__heap_end` is sufficient for
the initial top size to be a multiple of eight; alignment of `__heap_base`
alone is neither necessary nor sufficient for that conclusion. The candidate's
`__heap_end` alignment has not been established, nor have all later segment
and coalescing invariants. Until those branches are proved or excluded, the
alignment invariant and this charge remain provisional.
The specific unresolved branch is successful linker donation: `wasm.rs`
`preexisting_chunk_from_linker` returns `(__heap_base, __heap_end - __heap_base)`
when the nonzero ordered range is large enough, and `try_donate_preexisting`
returns it at most once (lines 17–31, 39–48, 88–98). No rounding or runtime
check of `__heap_end` occurs on that branch. The canonical manifest pins the
compiler, target and source archives, but is not itself a constraint on that
symbol in a linked candidate. As conditional linker evidence, upstream LLVM
22.1.8's wasm linker [rounds the final memory pointer to the configured page
size and assigns it to `__heap_end`](https://github.com/llvm/llvm-project/blob/llvmorg-22.1.8/lld/wasm/Writer.cpp#L430-L477).
Its [page-size option](https://github.com/llvm/llvm-project/blob/llvmorg-22.1.8/lld/wasm/Driver.cpp#L639-L644)
accepts both 65536 and 1: the former suffices, the latter does not establish
this alignment. The installed compiler reports LLVM 22.1.8, but that does not
bind the actual candidate linker to this upstream source or its effective
options. The candidate needs linker identity, options, symbol resolution and
artifact binding, or another proof of the condition. If donation is not taken,
`memory_grow` returns 64 KiB-page multiples; even its final-address-space
adjustment subtracts 16 bytes (lines 50–74, 121–123), preserving eight-byte
alignment.

The later paths have the following conditional induction, not an independent
base case (all line references below are to `dlmalloc.rs`):

- Contiguous extension of the segment containing top adds `tsize` to both
  segment and top sizes before `init_top` (lines 415–427). Growth sizes above
  preserve an aligned top size; they do not repair an unaligned one.
- A prepended segment (`sys_alloc` lines 430–438) aligns both new and old
  first chunk addresses. `prepend_alloc` computes their difference less `nb`
  and merges that remainder with top, the designated victim, or a free chunk
  (lines 745–777). Those additions preserve alignment only if the merged
  chunk already has it.
- A noncontiguous segment becomes top through `add_segment` and `init_top`
  (lines 787–813). Page-grown segment ends satisfy the same end-alignment
  condition. The old top's binned portion is the difference of aligned chunk
  addresses, or there is no such portion (lines 799–804, 837–844).
- `free` coalesces with the previous boundary-tag chunk, then with top,
  designated victim, or the next ordinary free chunk before binning
  (lines 1211–1283). Aligned sizes remain aligned under these additions;
  this cannot establish alignment when an earlier segment did not have it.
  The WASM system allocator refuses partial and whole releases (`wasm.rs`
  lines 105–114), so those release operations cannot alter this argument.

The small-bin exact/next-bin path
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
and a dependency-level conflicting-allocator fixture passed in the Worker
release job for the fixture change. That result does not certify a later candidate.
Reliance on allocator symbol names in the final bundle requires proof that the
build preserves those names; stripped names cannot prove absence.
The synthetic ledger CI build checks installed host components against the pinned
channel archives and invokes the verified Cargo and compiler paths with a fresh
output directory, then logs the resulting WASM digest. Its probes and build use
a minimal environment rather than inheriting arbitrary runner variables, and
known compiler and loader overrides are refused before verification. This is not
a build certificate: `/usr/bin` tools, dynamic loader inputs, dependency sources
and build scripts remain outside an authenticated, immutable build environment;
files may also change between checking and use. The digest alone does not bind
those inputs to the bytes. The ordinary candidate build is a different path and
has no coupled compiler, allocator, configuration and artifact evidence. Until
that boundary is established and the allocator argument is complete, nonempty
admission remains closed; no candidate artifact identity is claimed here.

Dispatch clones, response Value trees, rendering, database collection, and
platform buffers require their own accounting. This inventory alone does not
establish the complete 8 MiB graph or a concurrency guarantee. The AGENTS.md
Clean-room, YouTube and MetaBrainz guardrails remain applicable.
