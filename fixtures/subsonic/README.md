# Subsonic golden fixtures

Derived from v1 source (`routes/subsonic.php`, `SubsonicResponse`, the
`Subsonic/*` controllers/resources, `MergeFormPostParams`, Symfony
`JsonResponse`), not from live traffic: no v1 instance was available in
this environment, so every file is the exact byte body the Rust handler
returns for the seeded demo library below, with shapes verified field by
field against v1 source. The fixture suite
(`crates/nntune-subsonic/tests/fixture_suite.rs`) asserts byte equality.
A live capture against a seeded v1 instance remains future work; until
then these fixtures pin the port's v1-derived contract, not v1 bytes.
These 51 files are historical v1-derived pins, not live captures: the
live credential contract is versioned separately (see the
persisted-client-auth acceptance bank), while the `apiKey` 43/44,
missing-credential 40, and missing-parameter 10 golden bytes below
remain live.

## Envelope (v1 `SubsonicResponse`, ported exactly)

- XML (default `f`): `<?xml version="1.0" encoding="UTF-8"?>` + single
  `<subsonic-response>` root. Scalar values become attributes, maps become
  child elements, lists become repeated child elements named after the key,
  a `value` key becomes element text, bools render `true`/`false`, nulls are
  stripped. No pretty-printing.
- `f=json`: `{"subsonic-response":{...}}`, compact, native JSON types.
- `f=jsonp`: `/**/{callback}({...});` as `text/javascript` (Symfony
  `JsonResponse`, incl. the `/**/` sniffing guard); missing/empty/invalid
  `callback` falls back to JSON error 10 (as in v1). Callbacks must be
  dot-separated JS identifier paths with `[...]` indexers holding digits
  or quoted strings only (Symfony `setCallback` pattern + reserved words,
  ported exactly); unquoted `[name]`/`[a-b]` and reserved words are
  rejected.
- Envelope attributes: `status`, `version="1.16.1"`, `type="nntune"`,
  `serverVersion` (crate version), `openSubsonic="true"`. Errors add
  `<error code message/>` / `"error":{"code","message"}` with HTTP 200.

## Seeded demo library (in-memory `Store` used by the suite)

- Artist `ar-1` "Test Artist" (1 album); album `al-1` "Test Album" (1 song,
  200s, coverArt `al-1`); song `so-1` "Test Song" (track 1, 200s,
  `audio/mpeg`, genre Rock).
- Fixture users (enabled only in `seeded()` / `empty_with_test_auth()`;
  production `MemStore::empty()` accepts nothing): `test` /
  `test@example.com` (apiKey `test-key-1`, password `test-pass`), `other` /
  `other@example.com` (apiKey `other-key-1`), `manager` /
  `manager@example.com` (apiKey `mgr-key-1`, admin-or-manager roles),
  `admin` / `admin@example.com` (apiKey `admin-key-1`, `adminRole` true).
  Playlists, stars, ratings, scrobbles and queues are per-user; another
  user's playlist answers 50, and `getUser`/`getAvatar` answer 50 unless
  self or admin.
- 49 supported endpoints: the v1 authenticated table plus `tokenInfo`
  (reports the caller's username); `getOpenSubsonicExtensions` is public
  (no credentials needed) and claims exactly `apiKeyAuthentication` v1 +
  `formPost` v1; `songLyrics` is not claimed because the legacy `getLyrics`
  endpoint is still a text-only compatibility stub, per v1.
- `getUser` roles are v1 `UserResource` exactly (`settingsRole` true,
  `uploadRole`/`coverArtRole` for admin-or-manager, `commentRole` false,
  no `folder`); song entries never emit `bitRate`/`path`/`playCount` and
  always emit empty `transcodedContentType`/`transcodedSuffix`.
- `getArtists` and `getIndexes` include `ignoredArticles`; index letters strip
  those articles and place non-alphabetic names under `#`. `getIndexes` uses
  the v1 `indexes` wrapper and includes `lastModified`.
- `search3` returns `artist`, `album` and `song` legs, honors per-leg counts,
  and an empty query pages the whole library by `size`/`offset`.
- Album lists validate the v1 type set, require year bounds for `byYear` and
  a genre for `byGenre`, and cap page size at 500.
- `getLyricsBySongId` returns `lyricsList.structuredLyrics`; missing lyric data
  yields an empty collection, while an unknown song id remains error 70.
- Empty-library fixtures assert v1 behaviour with no rows: empty collections
  serialize to self-closed elements (`<artists/>`), never to 404.

## Error codes (Subsonic standard, as in v1)

- `0` refused ops (`jukeboxControl`, `getVideos`, `getVideoInfo`,
  `getCaptions`, `savePlayQueueByIndex`, 4 internet-radio ops) with
  `The {op} operation is not supported by this server.` — HTTP 200.
- `10` required parameter missing (also token auth without `s`, and an
  invalid JSONP callback, which falls back to JSON error 10 as in v1);
  `40` wrong username/password in the historical v1 source-derived
  contract (incl. malformed `enc:` hex; live, any `p` password is 42
  and complete token auth is 40);
  `43` apiKey mixed with `u`/`p`/`t`/`s`;
  `44` invalid API key; `50` is always
  `User is not authorized for the given operation.`;
  `70` uses the `SubsonicAwareErrorRenderer` generic text for `findOrFail`
  paths; data-not-found controllers preserve their endpoint-specific v1 text
  (for example `Song not found.` and `Album not found.`).
  `hls.m3u8` is not a Subsonic document: plain HTTP 410 (as in v1,
  matching Navidrome).
- Historical source-derived behavior only: `p=enc:<hex>` passwords
  hex-decoded as in v1; live, any `p` password answers 42 Unsupported.
  Query decoding is byte-based
  UTF-8 (no Latin-1 mojibake, no panic on stray `%`); POST form bodies merge
  with the query string as in v1 (`$request->input()`); POST bodies over
  10 MB are refused with error 10 `POST body exceeds the 10 MB limit.`
  before routing or auth (v1 `MergeFormPostParams::MAX_BODY_BYTES`).

## Byte routes

`stream`/`download` return `audio/mpeg` bytes for known ids (fixture asserts
status + content-type + length via the suite, not committed bytes);
`getCoverArt`/`getAvatar` return `image/png` bytes. `maxBitRate` and
`timeOffset` are accepted and ignored (no live transcoding, as in v1).
