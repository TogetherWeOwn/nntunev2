# Inert Subsonic snapshot construction accounting

The inert builder rejects all borrowed rows before allocation: database transfer
bounds do not prove allocation capacities or precharge retained rows, and even
an empty slice may have a retained backing allocation. No mount is enabled.

The Worker manifest pins Rust 1.98.1 and its wasm32 standard library. The
construction ledger charges each nonempty allocation using the pinned dlmalloc
rounding, minimum, overhead and remainder allowance; unsupported alignments and
checked arithmetic overflow are rejected. Charges are cumulative and never
refunded. This is a bound on covered application allocations, not on isolate
RSS, allocator free-list retention or runtime and driver memory.

| Owner or operation | Required charge and lifetime |
| --- | --- |
| Raw artist, release and track rows | The supplier must precharge backing vector capacities and every owned string capacity, including optional and otherwise unused fields, before allocation. The rows overlap mapping and tracks remain retained afterwards. A length or SQL octet limit is not a receipt. |
| Output artist, album and song vectors | One charge per nonempty vector for its proved requested element capacity times the target type size and alignment, before allocation. All three overlap the raw rows. |
| Output strings | One charge per nonempty copy, including album artist sort names, both song release-ID copies, and the fixed creation date, content type and suffix. Copies are retained by the output models. |
| Lookups and ordering | Borrowed scans and stable in-place ordering avoid separately allocated maps and sorting scratch; preserve last-row-wins lookups and equal-key occurrence order. Ordering rejects a caller-supplied comparison/probe work limit before mutation; string comparison cost and planner scans are not covered, so a future caller must also bound input lengths and establish an acceptable CPU limit. |
| Reallocation | Avoid growth beyond precharged capacity. If a covered path reallocates, precharge the new block while retaining the old charge; cumulative accounting does not refund the old block. |

For the pinned standard library, `alloc/src/raw_vec/mod.rs`'s
`try_allocate_in` requests `layout_array(capacity, elem_layout)` and records
the requested capacity; `grow_exact` records `len + additional` after a
checked addition. `alloc/src/alloc.rs`'s `Global::alloc_impl_runtime` returns
the requested layout size. These facts apply only to the pinned implementation
and the operations actually used; they do not establish a capacity bound for
arbitrary clones, collection growth or database decoding. The implementation
must reject absent raw-row provenance and must use and verify only covered
allocation paths before claiming successful construction.

Dispatch clones, response Value trees, rendering, database collection, and
platform buffers require their own accounting. This inventory alone does not
establish the complete 8 MiB graph or a concurrency guarantee. The AGENTS.md
Clean-room, YouTube and MetaBrainz guardrails remain applicable.
