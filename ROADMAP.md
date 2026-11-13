# NNTune v2 roadmap

Last reviewed: **2026-11-13 (UTC)**

Maintained by the project maintainers. For questions or suggestions, open a
[GitHub issue](https://github.com/TogetherWeOwn/nntunev2/issues).
Report security concerns privately through [SECURITY.md](SECURITY.md).

## How to read this roadmap

This is the public entry point for NNTune's product direction, not a delivery
tracker or a list of shipped features. The areas below describe intentions;
they are not release dates, availability guarantees or commitments to build
every item. Later work depends on validation, suitable data and licences,
privacy safeguards and release review.

Detailed accepted decisions remain in the [architecture decision records](docs/adr/README.md).
This summary does not replace those specifications or authorize a launch.
See [GitHub releases](https://github.com/TogetherWeOwn/nntunev2/releases) for
published release history, rather than inferring completion from this page.

## Product boundaries

NNTune is intended for invite-only listening from a shared, curated catalogue.
The listening model is FLAC first, then an official YouTube video embed where
a suitable video is available; otherwise the track is unavailable. Unavailable
tracks are hidden by default. YouTube remains a visible video feature, never
an audio-only or background-playback source, and it is never sold, upsold or
gated as a separate paid extra.

The initial scope is one concurrent stream per user and private playlists.
The staging subscription journey uses payment test mode; it does not activate
paid billing. Public availability and live billing require separate decisions
and release validation, not publication of this roadmap.

## Storage direction

The approved target is for NNTune to be a plain standard S3 client: an
endpoint, one bucket per environment and access keys, with a storage provider
owning Usenet access, encryption, caching, repair and retention. This is an
intention, not delivered: the merged implementation is unchanged and migration
will be staged, using disposable buckets until the provider is production-ready.
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
Genre browsing depends on licence-compatible data and adequate coverage of the
playable catalogue; mood labels are not assumed to have the same foundation.
Music-video discovery is conditional on validated links and the official embed
rules. Lyrics remain conditional on suitable sources and usage rights.

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
