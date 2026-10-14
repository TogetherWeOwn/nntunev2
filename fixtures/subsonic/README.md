# Subsonic golden fixtures

Captured from v1 (`routes/subsonic.php` + `SubsonicResponse`) behaviour, not
from live traffic: every file is the exact byte body the Rust handler must
return for the seeded demo library below. The fixture suite
(`crates/nntune-subsonic/tests/fixture_suite.rs`) asserts byte equality.

## Envelope (v1 `SubsonicResponse`, ported exactly)

- XML (default `f`): `<?xml version="1.0" encoding="UTF-8"?>` + single
  `<subsonic-response>` root. Scalar values become attributes, maps become
  child elements, lists become repeated child elements named after the key,
  a `value` key becomes element text, bools render `true`/`false`, nulls are
  stripped. No pretty-printing.
- `f=json`: `{"subsonic-response":{...}}`, compact, native JSON types.
- `f=jsonp`: `callback({...})`; missing `callback` falls back to JSON
  error 10 (as in v1).
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
  `other@example.com` (apiKey `other-key-1`), `admin` /
  `admin@example.com` (apiKey `admin-key-1`, `adminRole` true).
  Playlists, stars, ratings, scrobbles and queues are per-user; another
  user's playlist answers 50, and `getUser` answers 50 unless self or admin.
- Empty-library fixtures assert v1 behaviour with no rows: empty collections
  serialize to self-closed elements (`<artists/>`), never to 404.

## Error codes (Subsonic standard, as in v1)

- `0` refused ops (`jukeboxControl`, `getVideos`, `getVideoInfo`,
  `getCaptions`, `savePlayQueueByIndex`, 4 internet-radio ops) with
  `The {op} operation is not supported by this server.` — HTTP 200.
- `10` required parameter missing (also token auth without `s`, and an
  invalid JSONP callback, which falls back to JSON error 10 as in v1);
  `40` wrong username/password (incl. malformed `enc:` hex);
  `43` apiKey mixed with `u`/`p`/`t`/`s`;
  `44` invalid API key; `50` per-user forbidden;
  `70` data not found (unknown stream id: `Song not found.`).
  `hls.m3u8` is not a Subsonic document: plain HTTP 410 (as in v1,
  matching Navidrome).
- `p=enc:<hex>` passwords hex-decode as in v1; query decoding is byte-based
  UTF-8 (no Latin-1 mojibake, no panic on stray `%`); POST form bodies merge
  with the query string as in v1 (`$request->input()`).

## Byte routes

`stream`/`download` return `audio/mpeg` bytes for known ids (fixture asserts
status + content-type + length via the suite, not committed bytes);
`getCoverArt`/`getAvatar` return `image/png` bytes. `maxBitRate` and
`timeOffset` are accepted and ignored (no live transcoding, as in v1).
