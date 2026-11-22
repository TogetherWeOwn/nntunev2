# NNTune v2 agent notes

## Documentation map

- Start with the [roadmap](ROADMAP.md) for agreed product boundaries and conditional work.
- Read the [handoff](docs/handoff.md) for the dated source state, remaining work and published evidence.
- Use the [M1 traceability matrix](docs/m1-exit-traceability.md) and the
  [staging promotion checklist](docs/ops.md#staging-promotion-checklist) for acceptance and rollback.
- Consult the [architecture decision records](docs/adr/README.md) for design decisions and amendments.

## Working in T3 Code

The lead prioritizes and dispatches work, with up to four active task threads
per project. One task thread owns each PR-sized change in its own worktree,
from implementation through review, CI, merge and settlement. Continue an
existing PR on its branch. Use the PR watch for GitHub updates and settle the
thread when the work is complete.
[CONTRIBUTING.md](CONTRIBUTING.md#merge-policy) defines review
tiers, exact-commit evidence and merge authority.

Delegated implementation stays within the assigned worktree and file scope.
The task owner handles commits, pushes, PRs and integration. Implementation
helpers report changed files, dependencies, integration risks and verification,
including any checks they skipped. Search and triage helpers use read-only
inspection and report findings with file and line references, including where
they looked when evidence is missing.

The repository owner decides class-B design and policy changes. The owner
is the sole production approver and holds the Release Manager authority
named in the runbook and workflows. Stop and ask for explicit authorization
for the exact commit before any production action or any merge that could
reach production. This includes uncertain deployment paths, deploys,
migrations, secrets and grants. Follow the production admission and access
gates in [the operations runbook](docs/ops.md#staging-promotion-checklist).

Keep staging and production storage separate, and keep scratch data out of
archive buckets. Existing access, retention and deletion controls apply to
all environments. See the [storage boundary ADR](docs/adr/0015-standard-s3-storage-boundary.md)
and [bucket inventory](docs/ops.md#cloudflare).

### Model selection and effort

T3 Code selects the provider, model and effort for each thread through the
project default, a thread's launch selection or an independent reviewer's
target. The repository owner chooses the harness. Use the project default
when launching task threads and apply any task-specific model choice at
dispatch. Follow the independent cross-family review requirements in
[CONTRIBUTING.md](CONTRIBUTING.md#merge-policy) based on the model that ran
each review. Include [REVIEW.md](REVIEW.md) in each independent review brief.

- Use `claude-opus-5-5` whenever selecting Opus. Name that exact model ID
  in task, review and child briefs that request Opus, and set the effort
  explicitly. Use `medium` effort by default for tasks and reviews. Use
  `high` or `xhigh` only when the brief documents why the task needs it.
- Use `high` effort by default for Sol. Use `xhigh` only when the brief
  justifies it.
- Never use `max`, `ultra` or `ultrathink` on any model.

Keep model and effort policy here. Leave model selection to T3 Code and
keep repository harness configuration free of model pins, model-selection
environment variables and model-forcing hooks. The root `CLAUDE.md` imports
these shared instructions.

## Shell cleanup

Never use forced `rm`, including `rm -f`, `rm -rf` or `rm --force`. This
applies inside trap cleanups, `bash -lc` strings and all other wrappers.
Use `rm -r -- "$dir"` for a directory and `rm -- "$file"` for a file. If
cleanup is optional and the path might be absent, check for it first.

Report shell approval prompts to the task lead with the exact command
shape. Preserve the flags and wrapper and redact sensitive literals.

## CI and verification location

Full test suites and CI gates run on GitHub Actions, never as local
full-suite or CI-mirror commands on the shared host. Push the PR branch
and open a draft PR while incomplete; the actual workflow is
`.github/workflows/ci.yml` with the aggregate `ci-ok` gate.
[CONTRIBUTING.md](CONTRIBUTING.md#verification-github-actions-first)
defines focused reproduction, guarded Boat use and exact-head Actions
run/job evidence for reviewers and the owner. Local transcripts
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
  host-enforced filesystem quota, attested by the operator receipt in
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
  lease needing host operator inspection, a missing/bad pool, or low
  filesystem headroom. Retry later. Never fall back to a per-worktree
  `target/`, never set `CARGO_TARGET_DIR` manually, and never run a
  direct compiling `cargo` command to dodge the pool.
- A third concurrent invocation exits 75 immediately; it does not
  queue. Only `build`, `check`, `test`, `clippy` and `fmt` are
  accepted. Dependency metadata commands (`update`, `add`, `tree`)
  do not compile and run directly.
- One interrupted run keeps its `lease.json` sentinel; a stale
  under-budget lease with a dead Cargo process group is recovered on
  the next acquire, anything else stays for host operator inspection.

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
these rules and is awaiting retirement. The CI gate allow-lists that
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

## Modern web platform guidance

Before writing or changing browser-facing code in the SvelteKit app under `apps/web/` (`.svelte` components, CSS, HTML, client-side TypeScript), consult Modern Web Guidance. It covers current HTML, CSS and DOM APIs, accessibility, forms and performance. Skip it for server-only code, Rust crates and workers. The clean-room, YouTube and MetaBrainz guardrails above still win over any guide.

1. Run every Modern Web Guidance invocation with `DISABLE_TELEMETRY=1`
   and an environment limited to the runner settings shown below. Use the
   installed `modern-web-guidance` skill only if you can apply those controls
   and the same CLI version to its commands. Otherwise use the CLI directly
   (Node 22+):

   ```sh
   env -i PATH="$PATH" HOME="$HOME" DISABLE_TELEMETRY=1 \
     npx -y modern-web-guidance@1.0.1 search "<2-5 word capability>"
   env -i PATH="$PATH" HOME="$HOME" DISABLE_TELEMETRY=1 \
     npx -y modern-web-guidance@1.0.1 retrieve "<id>"
   ```

   Search one capability at a time (for example `"light dismiss dialog"`),
   without framework or app names.
2. Apply only the retrieved guidance that fits the task. Keep existing behavior, copy, design tokens and
   the stack the route already uses; record any guide you followed in the PR body.
3. The guidance never overrides this file, an ADR, or a project decision. If they
   conflict, follow the project and note the conflict in the PR.

## Contributing

PRs follow the org contract in `TogetherWeOwn/.github` CONTRIBUTING + this repo's overrides; use this repo's PR template, which replaces the org template; its Attestation section is mandatory.
