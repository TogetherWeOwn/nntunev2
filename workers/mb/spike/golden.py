#!/usr/bin/env python3
"""Build the release-search golden set for the R2 snapshot spike.

Usage: golden.py <curated-dir> <out-golden.tsv> [count] [seed] [--exclude prior.tsv ...]

No ingest query log exists yet (the staging ingest lane is gated), so the
golden set is derived from the dump itself through the ingest matcher's
own query path: a Usenet-style subject is built for each sampled release
and turned into the search `q` the way `workers/ingest/src/matching.rs`
does (`derive_upload_query` then `album_retrieval_query`; ported below,
pinned by tests/test_golden.py). Releases are sampled with a fixed seed,
in equal thirds by how many releases share the normalized title
(unique / 2-10 / more than 10), each with one subject variant:

- `plain`: `Artist - Title [01/10] - "01.flac" yEnc`
- `scene`: the same with ASCII-folded names and dots for spaces
- `underscore`: underscores for spaces
- `upper`: the plain subject uppercased

A hit is correct when it is any release whose normalized artist name and
title (the ingest matcher's `normalize_match_text`) equal the sampled
release's, the set the matcher's pick could accept. Output columns:
`id, q, variant, stratum, acceptable mbids (comma-separated)`.
"""

from __future__ import annotations

import argparse
import gzip
import random
import sys
import unicodedata
from collections import defaultdict
from pathlib import Path

NULL = r"\N"
FILE_TYPES = {"flac", "mp3", "ogg", "opus", "m4a", "wav", "rar", "zip", "7z", "par2", "nfo"}
VARIANTS = ("plain", "scene", "underscore", "upper")


def copy_text(field: str) -> str | None:
    """PostgreSQL COPY text decoding for the few escapes names carry."""
    if field == NULL:
        return None
    if "\\" not in field:
        return field
    out, i = [], 0
    simple = {"b": "\b", "f": "\f", "n": "\n", "r": "\r", "t": "\t", "v": "\v", "\\": "\\"}
    while i < len(field):
        ch = field[i]
        if ch != "\\" or i + 1 == len(field):
            out.append(ch)
            i += 1
            continue
        nxt = field[i + 1]
        out.append(simple.get(nxt, nxt))
        i += 2
    return "".join(out)


def normalize_match_text(raw: str) -> str:
    """Proxy ingest normalization; Python alnum omits some Alphabetic marks.

    See SNAPSHOT.md measurement limits before interpreting identity recall.
    """
    chars = list(raw.lower())
    out: list[str] = []
    apostrophe = ("'", "’")
    for i, c in enumerate(chars):
        if c == "n" and 0 < i < len(chars) - 1:
            before, after = chars[i - 1], chars[i + 1]
            if (not before.isalnum() and not after.isalnum()
                    and (before in apostrophe or after in apostrophe)):
                out.append(" n ")
                continue
        if c not in apostrophe:
            out.append(c if c.isalnum() else " ")
    return " ".join("".join(out).split())


def validate_match_query(raw: str) -> str | None:
    q = raw.strip()
    if not 2 <= len(q) <= 100 or any(unicodedata.category(c) == "Cc" for c in q):
        return None
    return q


def _is_counter(inner: str) -> bool:
    if inner.count("/") != 1:
        return False
    num, den = inner.split("/")
    return bool(num) and bool(den) and num.isascii() and den.isascii() and num.isdigit() and den.isdigit()


def _matching_open(text: str, open_ch: str, close_ch: str) -> int | None:
    depth = 0
    for index in range(len(text) - 1, -1, -1):
        if text[index] == close_ch:
            depth += 1
        elif text[index] == open_ch:
            depth -= 1
            if depth == 0:
                return index
            if depth < 0:
                return None
    return None


def _strip_trailing_counter(trimmed: str) -> str | None:
    if trimmed.endswith("]"):
        open_ch, close_ch = "[", "]"
    elif trimmed.endswith(")"):
        open_ch, close_ch = "(", ")"
    else:
        return None
    start = _matching_open(trimmed, open_ch, close_ch)
    if start is None:
        return None
    inner = trimmed[start + 1:-1]
    if _is_counter(inner):
        return trimmed[:start]
    if len(inner) >= 2 and ((inner[0] == "[" and inner[-1] == "]") or (inner[0] == "(" and inner[-1] == ")")):
        if _is_counter(inner[1:-1]):
            return trimmed[:start]
    return None


def derive_upload_query(subject: str) -> str | None:
    """Port of ingest `derive_upload_query` for one subject."""
    before_quote = subject.split('"')[0]
    candidate = before_quote.replace(".", " ").replace("_", " ").strip().rstrip("-").strip()
    while True:
        trimmed = candidate.rstrip()
        head = _strip_trailing_counter(trimmed)
        if head is None:
            parts = trimmed.rsplit(None, 1)
            head = parts[0] if len(parts) == 2 and parts[1].lower() == "yenc" else None
        if head is None:
            break
        nxt = head.strip().rstrip("-").strip()
        if nxt == candidate:
            break
        candidate = nxt
        if not candidate:
            break
    parts = candidate.rsplit(None, 1)
    if len(parts) == 2 and parts[1].lower() in FILE_TYPES:
        candidate = parts[0].strip()
    return validate_match_query(candidate)


def _fold_dashes(text: str) -> str:
    return "".join("-" if "‐" <= c <= "―" or c == "−" else c for c in text)


def album_retrieval_query(query: str) -> str | None:
    """Port of ingest `album_retrieval_query`: the album side of `A - B`."""
    folded = _fold_dashes(query)
    if " - " not in folded:
        return None
    artist, album = folded.split(" - ", 1)
    if not normalize_match_text(artist) or not normalize_match_text(album):
        return None
    return validate_match_query(album)


def ascii_fold(text: str) -> str:
    return "".join(c for c in unicodedata.normalize("NFKD", text) if not unicodedata.combining(c))


def subject_for(artist: str, title: str, variant: str) -> str:
    if variant == "scene":
        artist, title = ascii_fold(artist).replace(" ", "."), ascii_fold(title).replace(" ", ".")
        return f'{artist}.-.{title} [01/10] - "01.flac" yEnc'
    if variant == "underscore":
        artist, title = artist.replace(" ", "_"), title.replace(" ", "_")
        return f'{artist}_-_{title} [01/10] - "01.flac" yEnc'
    subject = f'{artist} - {title} [01/10] - "01.flac" yEnc'
    return subject.upper() if variant == "upper" else subject


def search_q(subject: str) -> str | None:
    """The `q` ingest sends for a subject (album side when it splits)."""
    query = derive_upload_query(subject)
    if query is None:
        return None
    return album_retrieval_query(query) or query


def read_tsv(path: Path):
    opener = gzip.open if path.suffix == ".gz" else open
    with opener(path, "rt", encoding="utf-8", newline="") as source:
        for line in source:
            yield line.rstrip("\n").split("\t")


def curated(directory: Path, table: str) -> Path:
    for name in (f"mbdump-{table}.tsv.gz", f"mbdump-{table}.tsv"):
        if (directory / name).is_file():
            return directory / name
    raise SystemExit(f"golden: missing curated {table} in {directory}")


def query_group(q: str) -> str:
    # Conservative grouping also excludes casing, accents and punctuation
    # variants of tuning queries, not just byte-identical query strings.
    return normalize_match_text(ascii_fold(q))


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("directory", type=Path)
    parser.add_argument("out", type=Path)
    parser.add_argument("count", type=int, nargs="?", default=1000)
    parser.add_argument("seed", type=int, nargs="?", default=3599)
    parser.add_argument("--exclude", type=Path, action="append", default=[],
                        help="Exclude query groups in this prior tuning TSV (repeatable)")
    args = parser.parse_args()
    if args.count <= 0:
        parser.error("count must be positive")
    directory, out, count = args.directory, args.out, args.count
    rng = random.Random(args.seed)
    excluded = {query_group(row[1]) for path in args.exclude for row in read_tsv(path)}

    names = {}
    for f in read_tsv(curated(directory, "artist")):
        name = copy_text(f[1])
        if name:
            names[f[0].lower()] = name
    releases = []
    title_freq: dict[str, int] = defaultdict(int)
    for f in read_tsv(curated(directory, "release")):
        mbid, artist_mbid, title = f[0].lower(), (copy_text(f[1]) or "").lower(), copy_text(f[2])
        if not title:
            continue
        norm_title = normalize_match_text(title)
        title_freq[norm_title] += 1
        artist = names.get(artist_mbid)
        if artist:
            releases.append((mbid, artist, title, norm_title))

    def stratum(norm_title: str) -> str:
        n = title_freq[norm_title]
        return "unique" if n == 1 else ("rare" if n <= 10 else "common")

    by_stratum: dict[str, list] = defaultdict(list)
    for release in releases:
        by_stratum[stratum(release[3])].append(release)
    picked = []
    per = count // 3
    for name in ("unique", "rare", "common"):
        pool = by_stratum[name]
        rng.shuffle(pool)
        want = per if name != "common" else count - 2 * per
        for release in pool:
            if want == 0:
                break
            variant = VARIANTS[len(picked) % len(VARIANTS)]
            q = search_q(subject_for(release[1], release[2], variant))
            if q is None or query_group(q) in excluded:
                continue
            picked.append((release, variant, name, q))
            want -= 1

    wanted_keys = {(normalize_match_text(r[1]), r[3]) for r, _, _, _ in picked}
    acceptable: dict[tuple[str, str], list[str]] = defaultdict(list)
    for mbid, artist, _title, norm_title in releases:
        key = (normalize_match_text(artist), norm_title)
        if key in wanted_keys:
            acceptable[key].append(mbid)
    with out.open("w", encoding="utf-8") as target:
        for index, (release, variant, name, q) in enumerate(picked, 1):
            key = (normalize_match_text(release[1]), release[3])
            target.write(f"{index}\t{q}\t{variant}\t{name}\t{','.join(sorted(acceptable[key]))}\n")
    print(f"golden: {len(picked)} queries from {len(releases)} releases with an artist", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
