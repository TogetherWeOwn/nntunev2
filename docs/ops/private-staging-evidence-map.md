# Private staging evidence map

This map is the offline slice of rank 113. It describes how the synthetic
private-music journey feeds the admission contract introduced by
[#1797](https://github.com/TogetherWeOwn/nntunev2/pull/1797). It does not
qualify a staging deployment or approve a product contract.

The verifier accepts an applicable private criterion only when its named
`private-music` JUnit case passes once on its first Playwright attempt
(`retry="0"`) within the selected Actions run attempt. The track-bearing cases
also carry one identical tuple through the report and receipt:

`track_id`, the SHA-256 of `tone-a.flac`, `build_sha`, `run_id`, and
`run_attempt`.

The cutoff refusal carries two additional random UUIDs that identify the
listener relationship without exporting a session binding. The verifier strips
those UUIDs before comparing the tuple. A different track, build, run, attempt,
or listener causes an incomplete receipt.

## Synthetic map

| Evidence area | Offline source and case | Verifier mapping | What the slice proves | Later staging gap |
| --- | --- | --- | --- | --- |
| Encrypted upload | `fixtures/synthetic-flac/private-staging/tone-a.flac`; `private-publication` under `upload-to-library` | `upload-to-library` | The checked-in FLAC digest is carried from a fresh publication receipt into the report and the receipt, and the canonical ID was absent before publication. | The fixture does not prove ciphertext at rest, envelope-key handling, immutable object completion, or provider range reads. Rank 77c must supply the protected encrypted-storage and upload evidence. |
| Library visibility | `private-library` under `upload-to-library` | `upload-to-library` | The wiring fixture carries the fresh canonical ID from publication into the `private-library` annotation. Exact-once Library cardinality is covered by the synthetic helper checks in [`track-evidence.spec.ts`](../../e2e/fixtures/private-music/track-evidence.spec.ts) and [`journey.spec.ts`](../../e2e/fixtures/private-music/journey.spec.ts). | A staging owner must show the real protected caller, database, storage version, and deployed build serving the row. |
| Playback | `private-play` under `play`, plus the `seek` and `queue` cases | `play`, `seek`, `queue` | The wiring fixture carries the fresh publication into `private-play`. Synthetic advancement, seek, resume, queue, and Next assertions are covered by [`journey.spec.ts`](../../e2e/fixtures/private-music/journey.spec.ts). Authenticated relay delivery and serving-byte agreement remain protected-staging checks. | The protected run must prove encrypted range delivery, media-byte agreement, metering, seek and queue behavior on the exact serving deployment. |
| Access refusals | `private-anonymous-refusal`, `private-unlisted-refusal`, and `private-cutoff-refusal` | `anonymous-refused`, `unlisted-refused`, `revoked-or-expired-refused` | The wiring fixture proves refusal annotation and listener-alias propagation, and fails closed for a switched listener or mismatched track. Synthetic status and row-clearing assertions are covered by [`journey.spec.ts`](../../e2e/fixtures/private-music/journey.spec.ts). | The protected staging journey must exercise the real invite, session, cohort cutoff, HTTP 401/403 Library and play responses, media-range refusal, and Library clearing, then retain only safe outcome evidence. |

The `invite-acceptance` and `video-off` criteria remain part of the full draft
contract. They are outside this four-area slice but still required for a
complete private journey. `paid-signup` remains explicitly excluded by the
draft contract.

## Build identity boundary

The offline harness uses synthetic test data only. Its 40-character build
strings, run number, attempt number, deployment UUIDs, canonical track UUIDs,
and listener UUIDs are generated or fixed inside the disposable fixture. They
must never be copied into a staging readout as serving identifiers.

The exact-build requirement is represented by the two synthetic identity cases
`build-identity` and `build-identity-final`. Each reports the same test SHA for
Web, API, and the expected value. The protected journey must replace those
test values with readbacks from the same Web, API, and Stream deployment, then
retain the attempt-bound admission artifact. A source commit, a green job, or
an offline receipt alone is not exact-build staging evidence.

## Offline check

The bounded wiring fixture exercises the report, receipt, shared provenance tuple,
and listener-alias paths in this map:

```sh
python3 -B -I e2e/fixtures/private-music/test_admission_wiring.py "$(command -v node)"
```

It reads only checked-in synthetic media and writes reports inside a disposable
directory. The command requires Bash, Git, Python, `jq`, Node, and the pinned
`e2e` dependencies installed, as in the hosted `e2e-lint` job. The focused verifier tests in
`ci/test_staging_admission.py` additionally reject mixed tuples, duplicate or
missing private proofs, wrong listener identity, and unapproved contracts.
