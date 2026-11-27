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

The track properties below correspond in order to the listed verifier cases.
The `seek` and `queue` cases require a pass without a track property.

| Evidence area | Track properties | Verifier cases | Offline coverage | Later staging gap |
| --- | --- | --- | --- | --- |
| Encrypted upload | `private-publication` | `upload-to-library` | The wiring fixture ties the digest of `fixtures/synthetic-flac/private-staging/tone-a.flac` to a synthetic publication receipt with a canonical ID absent from the supplied prior IDs. Ciphertext-at-rest verification requires protected storage evidence. | Rank 77c must supply protected proof of encrypted upload, including ciphertext at rest, envelope-key handling and immutable object completion. |
| Library visibility | `private-library` | `upload-to-library` | The wiring fixture carries the fresh track ID into the Library annotation. [`track-evidence.spec.ts`](../../e2e/fixtures/private-music/track-evidence.spec.ts) checks Library cardinality with synthetic rows. | Verify that the protected caller publishes the pinned storage version and the invitee sees that exact track once after Library Refresh on the serving build. |
| Playback | `private-play` | `play`, `seek`, `queue` | The wiring fixture carries the fresh publication into the play annotation. [`journey.spec.ts`](../../e2e/fixtures/private-music/journey.spec.ts) uses synthetic observations to check advancement, paused seek, resume, queue order and Next. | Verify authenticated relay playback, exact nonzero Range bytes, metering, seek and queue behavior on the pinned deployments. |
| Access refusals | `private-anonymous-refusal`, `private-unlisted-refusal`, `private-cutoff-refusal` | `anonymous-refused`, `unlisted-refused`, `revoked-or-expired-refused` | The wiring fixture checks refusal annotations and cutoff listener correlation with synthetic sessions. [`journey.spec.ts`](../../e2e/fixtures/private-music/journey.spec.ts) checks refusal statuses and Library clearing with synthetic inputs. | Verify 401 for signup without an invite, anonymous Library access and anonymous play. Verify 403 for unlisted and cutoff Library and play, plus media-range denial and Library clearing for the cutoff session. |

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
