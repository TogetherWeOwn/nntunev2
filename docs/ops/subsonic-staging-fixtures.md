# Inert Subsonic staging fixtures

These recipes contain only authored synthetic data. The regular mount remains
unchanged and off. In a separately authorized measurement window, the existing
trusted runner selects `SUBSONIC_MEASUREMENT_FIXTURE` in deployment config, not
in the request. The lease, runner and authenticated fixture principal are still
required. Nothing here arms a window or accesses a catalogue database.

## Fixture-to-boundary map

Each triplet uses the suffixes `below`, `exact`, `over` in that order. Sizes are
uncompressed UTF-8 response bytes. `A` is artist ID
`00000000-0000-4000-8000-000000000001`; `R` is release ID
`00000000-0000-4000-8000-000000000002`. Request paths below are under `/rest/`.

| Fixture IDs | Boundary inputs | Request | Expected HTTP status / bytes |
| --- | --- | --- | --- |
| `rows-*` | 1,023 / 1,024 / 1,025 track rows (one artist and release) | `getArtist?id=A&f=json` | 200 / 492; 200 / 492; 503 / 112 |
| `field-*` | 1,023 / 1,024 / 1,025 title bytes | `getAlbum?id=R&f=json` | 200 / 1,991; 200 / 1,992; 503 / 112 |
| `text-*` | 262,143 / 262,144 / 262,145 raw field bytes across 258 rows | `getArtists?f=json` | 200 / 296; 200 / 296; 503 / 112 |
| `output-json-*` | 262,143 / 262,144 / 262,145 encoded bytes | `getAlbum?id=R&f=json` | 200 / 262,143; 200 / 262,144; 413 / 122 |
| `output-xml-*` | Same, including XML escaping | `getAlbum?id=R` | 200 / 262,143; 200 / 262,144; 413 / 130 |
| `output-jsonp-*` | Same, including wrapper and callback | `getAlbum?id=R&f=jsonp&callback=fixture.cb` | 200 / 262,143; 200 / 262,144; 413 / 122 (plain JSON) |
| `large-mixed` | One artist, four releases, 512 track occurrences; 256-byte UTF-8 titles with repeated escaping | First release via the three `getAlbum` requests above | 200 / 106,659 (JSON), 118,091 (XML), 106,676 (JSONP) |

`empty` and `small` are unchanged. The short `rows-*` and `text-*` projections
intentionally avoid the independent output cap while building the full snapshot.
Row/text/charge preflight runs before raw row reservation; the existing counted
renderer rejects over-cap output before allocating the response body.

**Actions only:** the `worker-release-builds` WASM harness uses `large-mixed`
with raw-row ledger limits of 291,929 / 291,928 / 291,927 bytes (admit / admit /
reject before reserving rows, no HTTP body). The staging limit is not lowered
by config. `api-migrations` separately tests PostgreSQL membership, mirror
row/text preflight, and transfer/transaction behavior using disposable databases,
not a shared mirror. These are application limits, not isolate memory or
response-lifetime guarantees.
