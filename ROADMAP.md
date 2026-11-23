# NNTune v2 roadmap

Last reviewed: **2026-11-21 (UTC)**

Maintained by the project maintainers. For questions or suggestions, open a
[GitHub issue](https://github.com/TogetherWeOwn/nntunev2/issues).
Report security concerns privately through [SECURITY.md](SECURITY.md).

## How to read this roadmap

This is the public entry point for NNTune's product direction, not a delivery
tracker or a list of shipped features. The areas below describe intentions;
they are not release dates, availability guarantees or commitments to build
every item. Later work depends on validation, suitable data and licences,
privacy safeguards and release review.

Find accepted decisions and amendments in the [architecture decision records](docs/adr/README.md).
The [M1 matrix](docs/m1-exit-traceability.md) defines the staging exit and maps
its checks. The [handoff](docs/handoff.md) records source and staging evidence as
of a named revision. See [GitHub releases](https://github.com/TogetherWeOwn/nntunev2/releases)
for published releases.

## Product boundaries

NNTune is intended for invite-only listening from a shared, curated catalogue.
The listening model is FLAC first, then an official YouTube video embed where
a suitable video is available; otherwise the track is unavailable. Unavailable
tracks are hidden by default. YouTube remains a visible video feature, never
an audio-only or background-playback source, and it is never sold, upsold or
gated as a separate paid extra.

The initial scope is one concurrent stream per user and private playlists.
The [accepted plan matrix](docs/adr/0004-plan-matrix.md) specifies no trial or
free plan. At the monthly stream-hour cap, new playback is refused while a
track already playing can finish. Staging uses payment test mode. Public
availability and live billing require separate decisions and release validation.
M1 uses owner-issued invites. The later scope allows eligible paid members to
invite during owner-scheduled windows.

## Storage direction

The approved target is for NNTune to be a plain standard S3 client: an
endpoint, one bucket per environment and least-privilege access keys. The
provider owns storage-side Usenet access, at-rest encryption, caching, repair
and retention; NNTune has no NZB, NNTP or Usenet awareness in that target.

Curated audio files are owner-supplied through an authenticated programmatic
ingest API for scripts, external services and apps, with scoped credentials,
idempotency keys and multipart/presigned transfer of ciphertext only. NNTune
keeps an application-side plaintext SHA256-to-object-key dedupe map and encrypts
every upload before S3, whatever the provider. Mandatory client-side envelope
encryption uses a Worker-secret key-encryption key and seekable chunked AEAD;
the stream Worker decrypts authenticated chunks for ranged playback and keeps
metering. This is not end-user end-to-end encryption: authorized Workers can
decrypt. Upload checksums cover ciphertext; plaintext and keys stay out of logs.

Implementation has landed in bounded increments. See the
[storage handoff](docs/handoff.md#storage-and-catalogue) for what is present
in source and what remains before activation. ADR 0015 retains the exact-design
Security, Applied Cryptography and QA gates and requires verified producer
outputs before downstream reactivation. No plaintext fallback or hidden
valid-playback/library cap is allowed.
Migration stays staged, using disposable buckets until the provider is
production-ready. Retain legacy readers and inventory, coexistence, restore
and rollback obligations until the accepted replacement is ready. The replacing
ingest delivery must retire the obsolete NZB fetch/verify/repair path, not
leave it as a fallback.
See [ADR 0015](docs/adr/0015-standard-s3-storage-boundary.md).

## Direction

### Reliable everyday listening

Focus on a coherent desktop and mobile web experience: browse the library,
play and seek, manage a queue, save favourites and resume listening. Keyboard
and screen-reader access, understandable loading/error states and honest
availability information belong in the core experience, not as later polish.
Staging validation must cover the invite, test-subscription and curated-track
listening journey before a production launch is considered.

### Find and organise the catalogue

Develop search, album and artist pages, useful credits and library organisation.
Private playlist creation, editing, ordering and playlist-file import should
make a personal collection easy to manage without introducing public sharing.
Album playback continuity and compatible-client access are further intentions
within this area, not claims of current support.

### Discovery grounded in real data

Build towards useful listening-history views and optional ListenBrainz-based
discovery, with meaningful fallbacks when history or linked data is absent.
Genre browsing depends on licence-compatible data and coverage of the
playable catalogue. Mood labels need their own data assessment.
Music-video discovery is conditional on validated links and the official embed
rules. The [embed decision](docs/adr/0012-compliant-youtube-video-and-brainz-data.md)
records the launch legal question and the historical spike's flag-off status.
Use [ADR 0007's amendment](docs/adr/0007-mb-mirror.md#2026-11-07-amendment-retirement-and-conditional-bounded-replacement)
for the weekly core-CC0 mirror refresh plan. Genre browsing needs
[measured coverage of at least 80% of playable artists](docs/genre-root-coverage.md#later-real-data-measurement-and-guardrails).

Lyrics depend on suitable sources and usage rights. The
[handoff](docs/handoff.md#player) records the existing panel and the activation
work that remains. See the [attribution inventory](docs/attributions.md#scope-and-provenance).

### Later, conditional possibilities

Cross-device resume and handoff should move the single stream, not multiply
it. Optional social and collaborative features remain later possibilities,
subject to privacy controls and a separate decision on sharing. They do not
change the initial private-playlist boundary.

Audio leveling, verified played-rendition quality details and speculative
next-track prefetch are deferred requirements, not delivered capabilities.
Keeping helpers or displaying a source type does not establish that these
requirements are met. A future data-saving stream or quality selector is also
outside the initial FLAC-first scope.

## Non-goals

- No offline music downloads or persistent offline media copies.
- No user Usenet/provider credentials, provider setup wizard, or user NZB
  uploads or URL intake. Listeners use the curated catalogue.
- No public playlists or share links in the initial scope.
- No hidden, extracted, cached or offline YouTube media.
- No advertising platform, music store, or open-signup launch implied by this
  roadmap.

These boundaries take precedence over feature wish lists. Suggestions are
welcome through the issue tracker, but an issue or roadmap edit alone does
not expand the accepted product scope.
