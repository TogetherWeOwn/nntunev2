# NNTune v2 agent notes

## Documentation map (repo-only, resumable from this checkout)

- Product direction, approved vs proposed scope and non-goals:
  [ROADMAP.md](ROADMAP.md) (entry point; use it first for scope).
- Active milestone requirements and acceptance criteria:
  [docs/specs/](docs/specs/README.md) (M1 exit) and
  [docs/m1-exit-traceability.md](docs/m1-exit-traceability.md).
- Approved approach with implemented vs remaining and verification
  routes: [docs/plans/](docs/plans/README.md).
- Material owner decisions with provenance:
  [docs/decisions/](docs/decisions/README.md).
- Deploy, rollback and recovery procedures:
  [docs/runbooks/](docs/runbooks/README.md) and [docs/ops.md](docs/ops.md).
- Dated current source snapshot and next unfinished step:
  [docs/handoff.md](docs/handoff.md).
- Technical decisions: [docs/adr/](docs/adr/README.md).

Assignment and live-status tracking stays in the task tracker; the
repo docs above carry the durable knowledge.

## CI and verification location

Full test suites and CI gates run on GitHub Actions, never as local
full-suite or CI-mirror commands on the shared host. Push the PR branch
and open a draft PR while incomplete; the actual workflow is
`.github/workflows/ci.yml` with the aggregate `ci-ok` gate.
[CONTRIBUTING.md](CONTRIBUTING.md#verification-github-actions-first)
defines focused reproduction, guarded Boat use and exact-head Actions
run/job evidence for reviewers and the Release Manager. Local transcripts
are not CI evidence; required checks and review/release gates stay intact.

Local test execution is allowed only for focused reproduction of one
failing test or crate. Heavy focused work uses the guarded `boat` PATH
wrapper with sizing/budget/security controls, `--no-env`, a source-only
export and verified cleanup. Boat fallback never permits a full suite,
CI mirror or heavy raw-host build; defer to Actions if a permitted
focused fallback is unavailable.

## Local Rust builds: the bounded Cargo pool

Only small focused Rust reproductions belong in this host pool. Compiling
Cargo commands never use a per-worktree `target/`. They go through the
bounded slot pool in `scripts/cargo_cache.py`, either directly or via the
host `cargo` shim (which routes automatically when that file exists):

```sh
# Copy both the package and fully qualified harness test name from the failing Actions job.
python3 scripts/cargo_cache.py run -- test -p <failing_package> <fully_qualified_test_name> -- --exact
# Or reproduce the one failing standalone crate, not every manifest:
python3 scripts/cargo_cache.py run -- test --manifest-path <failing_crate_manifest>
```

Replace the placeholders before running. Use the full module-qualified name
(for example, `transcode::tests::ffmpeg_argv_is_pinned`), not just the
function name. If the job selects a feature or test target, retain that
selection for the failing test. Confirm the intended test actually ran:
zero matching tests is not a successful reproduction, even with exit 0.
For a standalone-crate failure, take its manifest path from that Actions job.

Keep any `check`, `clippy` or `build` reproduction scoped to that same
failing crate; heavy work goes through Boat, not this pool. `run -- fmt`
is allowed; bare `cargo fmt` does not compile and may run directly, but
prefer the wrapper for consistency. The pool is a resource guard, not
permission for broad local verification or a full-suite fallback.

Pool facts:

- Two slots, each with a sampled byte budget (see
  `scripts/cargo_pool_policy.json`, provisioned on the host under
  `/paperclip/.cache/nntunev2-bounded`). The hard aggregate bound is a
  host-enforced filesystem quota, attested by the Operator receipt in
  the pool policy — not by the script.
- The wrapper adds `--offline --locked`, redirects `CARGO_TARGET_DIR`
  and `TMPDIR` into per-worktree subdirs (`ws-<hash>`) under the leased
  slot, disables incremental compilation and dev/test debuginfo in the
  child environment only, and rejects output/config overrides and
  manifests outside the workspace. Acquire evicts other worktrees'
  build-output-only subdirs to preserve the slot budget (protected
  material refuses the slot, leaving it unchanged); legacy
  shared-target slots migrate automatically after the same check.
- Exit 75 means refused: both slots busy, a slot over budget, a stale
  lease needing Operator inspection, a missing/bad pool, or low
  filesystem headroom. Retry later. Never fall back to a per-worktree
  `target/`, never set `CARGO_TARGET_DIR` manually, and never run a
  direct compiling `cargo` command to dodge the pool.
- A third concurrent invocation exits 75 immediately; it does not
  queue. Only `build`, `check`, `test`, `clippy` and `fmt` are
  accepted. Dependency metadata commands (`update`, `add`, `tree`)
  do not compile and run directly.
- One interrupted run keeps its `lease.json` sentinel; a stale
  under-budget lease with a dead Cargo process group is recovered on
  the next acquire, anything else stays for Operator inspection.

## Clean-room, YouTube and MetaBrainz guardrails

These are acceptance criteria for every UI, playback and catalogue
change. Cite this section from the change description when a change
touches any of them. The mechanically checkable half is enforced by
`.github/scripts/clean_room_lint.py` in CI; the rest is review
discipline, and a reviewer may block on any line below.

### Clean room (competitor-inspired UI work)

Analysis of existing products is an input to *written specs* — layout,
measurements, spacing, motion, states and features described in our
own words. NNTune code, styles and copy are then written fresh from
those specs.

- Never bring into this repo, a commit, a PR or any public text:
  competitor code or CSS, class names, CSS custom-property names,
  icons, images, logos or wordmarks, UI copy text, or proprietary
  fonts. For Spotify specifically that includes the name (outside
  prohibition statements like this one), the greens `#1DB954` and
  `#1ED760`, and the typefaces Spotify Mix and Circular.
- Analysis evidence (extracts, bundles, traces, screenshots) stays
  outside every repo and worktree. Never commit it, and never write
  its host path or directory names into the repo — a reference is a
  leak of what was analysed and where it sits.
- Specs may state measurements and behaviour; they must not quote
  competitor source, string tables or asset names.
- Use our own brand throughout: our accent colour, an openly licensed
  font (for example Figtree or `system-ui`), and openly licensed icons
  (for example Lucide or Phosphor). No "-ify"-style naming.
- Never sign in to Spotify, Deezer, Tidal or Apple Music with anyone's
  account for analysis or testing.

### YouTube (embed-only)

YouTube playback is the official IFrame embed, presented as a video
feature — never as an audio source.

- Visible official IFrame player only, at least 200×200 (480×270
  recommended), never covered or overlaid; YouTube's controls, ads
  and branding stay untouched.
- At most one autoplaying player, and only while more than half of it
  is in view. Playback pauses when the page or player is hidden.
- No audio-only playback, no background, hidden, minimised or
  lock-screen playback.
- No stream extraction and no direct media URLs (`googlevideo`,
  `/youtubei/`, yt-dlp and equivalents are banned by
  `clean_room_lint.py`).
- No caching or offline copies of YouTube content.
- Never charge separately for watching the embed, upsell it, or gate
  it beyond access to the service itself (no step past the play
  click except login/age checks, no rewards for watching). There is
  no free plan at launch (ADR 0004), so ADR 0012 §YouTubeEmbedSource
  restates III.F.3.a for an invite-only, subscription-gated service.
  Whether that reading satisfies III.F.3.a is a launch-gate legal
  item, not a settled permission; do not add a free-plan embed path
  or claim the item resolved without a product/legal decision.
- Never call `search.list` at play time. Resolve video IDs ahead of
  time and validate them in `videos.list` batches. Storing video IDs
  is fine; any other stored API metadata is refreshed or deleted
  within 30 days.
- The product links the YouTube Terms of Service and the Google
  Privacy Policy wherever the embed is offered.

The pre-existing extraction spike under `workers/youtube/` predates
these rules and is awaiting retirement (kept, flag-off — see
[docs/decisions/0001-youtube-embed-first.md](docs/decisions/0001-youtube-embed-first.md)).
The CI gate allow-lists that
path (and its design note) for the extraction rule only, temporarily.
Do not extend the spike, and do not copy its approach into new code.

### MetaBrainz licences (MusicBrainz, ListenBrainz, CritiqueBrainz, cover art)

- MusicBrainz **core data is CC0** — including URL relationships —
  and may be mirrored and shipped, commercial use included.
- **Supplementary data is CC BY-NC-SA**: tags/genres, ratings,
  annotations, and the hourly Live Data Feed replication packets.
  NC-licensed data never ships in this paid product and must not be
  ingested into the product mirror; keep the mirror CC0-only unless
  a licence decision says otherwise.
- CritiqueBrainz: use only **CC BY-SA** reviews, with attribution.
  Skip any review under an NC variant.
- **MLHD+ is never used** — not in the product, not in tooling.
- Cover art (Cover Art Archive and similar) carries third-party
  rights that the archive does not clear. Treat every cover-art use
  as rights-flagged: reference or proxy art deliberately, never
  assume a licence to redistribute, and surface new cover-art caching
  for review before it ships.
- ListenBrainz dumps are CC0 and may be used on the same terms as
  MusicBrainz core data.
- Respect the MusicBrainz API etiquette for any live calls: at most
  1 request/second per IP and a descriptive `User-Agent` of the form
  `nntune/<version> ( <contact> )`. Serve product reads from the
  mirror, not the live API.

## Contributing

PRs follow the org contract in `TogetherWeOwn/.github` CONTRIBUTING + this repo's overrides; use this repo's PR template, which replaces the org template; its Attestation section is mandatory.
