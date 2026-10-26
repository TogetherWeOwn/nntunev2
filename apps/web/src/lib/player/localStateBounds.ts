/**
 * Canonical bounds for demo-local player-state decoding (resume positions and
 * recently-played history only).
 *
 * These limits are measured BEFORE `JSON.parse` on the raw stored string and
 * fail closed: over-cap or malformed payloads decode to empty/default without
 * throwing, logging, or touching unrelated keys. Writer output always stays
 * readable under the same bounds via deterministic refusal (no implicit
 * eviction, no partial writes).
 *
 * Reconciliation: ordinary fixtures in the sibling protected suites are orders
 * of magnitude under these bounds (demo ids <= 13 chars, payloads < 1 KiB),
 * so valid behavior is preserved bit-for-bit.
 */

/** Max raw serialized chars (inclusive, untrimmed) for either payload. */
export const MAX_SERIALIZED_CHARS = 65_536;

/**
 * Max own enumerable string keys in a parsed resume object (valid or not).
 * Over-budget payloads are refused wholesale, not truncated.
 */
export const MAX_RESUME_ENTRIES = 256;

/**
 * Max opaque track/recording id length in code units (`String.length`
 * semantics). Longer ids are ignored on write and treated as absent on read.
 */
export const MAX_OPAQUE_ID_CHARS = 256;
