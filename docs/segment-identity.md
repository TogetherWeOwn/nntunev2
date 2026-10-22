# Segment identity contract

One shared rule governs every segment message-id at every stage. The rule
itself is defined and documented once, in code, at
`crates/nntune-core/src/segment_identity.rs`
(`MAX_SEGMENT_ID_BYTES` + `valid_segment_id`); this note is the map, not
a second definition. If the two ever disagree, the code wins.

## The rule

- 1..=250 bytes (the NNTP wire ceiling), printable ASCII (`0x21..=0x7E`)
  only, no `<`/`>`, exactly one `@` with non-empty sides (RFC 5536
  `msg-id`, bracketless — the wire layer adds `<…>`).
- No normalization: ids are validated byte-for-byte as given. Outer
  whitespace, `<…>` brackets, or any other refused spelling is refused,
  never trimmed or unbracketed into an accepted identity, so every
  spelling is treated identically at intake, probe/body, play, and wire.

## Where it is enforced

| Stage | Seam | Refusal |
|---|---|---|
| NZB structural parse | `nntune_core::nzb::parse` (strict bare check, 998-byte structural bound) | `Error::Malformed` |
| Ingest intake | `parse_upload` / `parse_upload_files` cross-check | `ParseRejection::Invalid` |
| Ingest envelopes | `build_probe_request` (+ ranges), `build_body_request` | `Err(&'static str)` |
| Pool probe/body parsers | stream `parse_request` / `parse_body_request` | 400 `bad_request` |
| Play | pre-lease segment-list validation | 400 `invalid_request`, non-retryable |
| Native wire | `NntpClient::article` (defense in depth) | `NntpError::InvalidMessageId`, zero wire bytes |

Each stage re-validates on receipt; the rule is shared, the checks are
not skipped. A wire-refused id is refused at intake and the envelopes
before any pool lease or dial.

## Empty tracks

An empty segment list at play is a local metadata verdict —
`PlayError::InvalidSegment` → 400 `invalid_request`, non-retryable, no
`Retry-After` — with no lease, dial, quarantine, or provider-error
meter. Genuine upstream failures (dial errors, bad replies) still map to
retryable 502 `upstream_unavailable`.
