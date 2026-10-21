#!/usr/bin/env bash
# Day-1 corpus builder: 50 VEVO label music videos + 50 "- Topic" audio tracks.
#
# Phase 1 harvests video IDs from YouTube search HTML (plus known-good
# seeds). Phase 2 bootstraps Topic tracks from the related videos of
# already-found Topic watch pages (search relevance buries Topic uploads;
# related rails are ~45% Topic). Each candidate is verified + classified
# through oEmbed (no API key, cacheable endpoint):
#   author contains "VEVO"      -> label_music
#   author ends with " - Topic"  -> audio_track
#   anything else / oEmbed miss -> skipped (counted, not stored)
#
# RETIRED (host research 2026-10-21): VEVO/"- Topic" substring matching is
# banned as a selection method. This script fails closed until rewritten
# to the OMV/ATV method in SOURCES.md; the body below is kept for
# harvest-reference only.
# Usage: ./build.sh [output.json]   (default: day1.json next to this script)
# Requires: bash 4+, curl, jq. Re-run to refresh; entries carry
# verified_at + source. Exits non-zero unless both classes reach 50.
set -euo pipefail

echo "build.sh: retired — substring selection is banned (see SOURCES.md); refusing to write a corpus" >&2
exit 75

OUT="${1:-$(dirname "$0")/day1.json}"
UA="Mozilla/5.0 (Macintosh; Intel Mac OS X 15_7_3) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/26.0 Safari/605.1.15"
TODAY="$(date -u +%Y-%m-%d)"
WANT=50
BOOTSTRAP_FETCH_CAP=15

LABEL_Q=(
  "vevo official music video"
  "official music video"
  "official video vevo 2025"
  "official video vevo 2024"
  "pop official music video"
  "hip hop official music video"
  "rock official music video"
  "latin official music video"
)
TOPIC_Q=(
  "provided to youtube"
  "official audio"
  "full album provided to youtube"
  "lyric video official audio"
  "official audio pop"
  "official audio rock"
  "official audio hip hop"
  "deep cuts official audio"
)
# Known-good label seeds (oEmbed still verifies each; dead ones drop out).
SEEDS=(dQw4w9WgXcQ 9bZkp7q19f0 JGwWNGJdvx8 JGgWNGJdvx8 RgKAFK5djSk e-ORhEE9VVg kJQP7kiw5Fk OPf0YbXqDm0 60ItHLz5WEA hTWKbfoikeg fJ9rUzIMcZQ)

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
: > "$TMP/kept.jsonl"

declare -A SEEN=()
label=0; topic=0; skipped=0

harvest_search() {
  local q="$1"
  curl -s --max-time 25 -A "$UA" -G "https://www.youtube.com/results" \
    --data-urlencode "search_query=$q" \
    | grep -o '"videoId":"[A-Za-z0-9_-]\{11\}"' \
    | cut -d'"' -f4 || true
}

harvest_related() {
  local id="$1"
  curl -sL --max-time 30 -A "$UA" "https://www.youtube.com/watch?v=${id}" \
    | grep -o '"videoId":"[A-Za-z0-9_-]\{11\}"' \
    | cut -d'"' -f4 || true
}

# Classify one candidate; echoes the class ("label_music", "audio_track"
# or "skip") and records kept entries. Seen-set dedupes across phases.
classify() {
  local id="$1" source="$2"
  [[ -n "${SEEN[$id]:-}" ]] && { echo "skip"; return; }
  SEEN[$id]=1
  local body author title vtype
  body="$(curl -s --max-time 15 "https://www.youtube.com/oembed?url=https://www.youtube.com/watch?v=${id}&format=json" || true)"
  author="$(printf '%s' "$body" | jq -r '.author_name // empty' 2>/dev/null || true)"
  title="$(printf '%s' "$body" | jq -r '.title // empty' 2>/dev/null || true)"
  vtype="$(printf '%s' "$body" | jq -r '.type // empty' 2>/dev/null || true)"
  [ "$vtype" = "video" ] || { skipped=$((skipped+1)); echo "skip"; return; }
  local cat=""
  case "$author" in
    *VEVO*) [ "$label" -lt "$WANT" ] && { cat="label_music"; label=$((label+1)); } ;;
    *" - Topic") [ "$topic" -lt "$WANT" ] && { cat="audio_track"; topic=$((topic+1)); } ;;
  esac
  if [ -z "$cat" ]; then skipped=$((skipped+1)); echo "skip"; return; fi
  jq -cn --arg id "$id" --arg cat "$cat" --arg t "$title" --arg a "$author" \
    --arg d "$TODAY" --arg s "$source" \
    '{video_id:$id,category:$cat,title:$t,author:$a,verified_at:$d,source:$s}' >> "$TMP/kept.jsonl"
  echo "$cat"
}

full() { [ "$label" -ge "$WANT" ] && [ "$topic" -ge "$WANT" ]; }

# Phase 1: seeds + search.
for id in "${SEEDS[@]}"; do full && break; classify "$id" "seed" > /dev/null; done
for q in "${LABEL_Q[@]}" "${TOPIC_Q[@]}"; do
  full && break
  while IFS= read -r id; do full && break 2; classify "$id" "search" > /dev/null; done < <(harvest_search "$q")
done
echo "phase1: label_music=$label audio_track=$topic skipped=$skipped" >&2

# Phase 2: bootstrap Topic tracks from related rails of found Topic videos.
fetches=0
while [ "$topic" -lt "$WANT" ] && [ "$fetches" -lt "$BOOTSTRAP_FETCH_CAP" ]; do
  progress="$topic"
  while IFS= read -r seed; do
    [ "$topic" -ge "$WANT" ] && break 2
    [ "$fetches" -ge "$BOOTSTRAP_FETCH_CAP" ] && break 2
    fetches=$((fetches+1))
    while IFS= read -r id; do
      [ "$topic" -ge "$WANT" ] && break 3
      classify "$id" "watch-bootstrap" > /dev/null
    done < <(harvest_related "$seed")
  done < <(jq -r 'select(.category=="audio_track") | .video_id' "$TMP/kept.jsonl" | sort -u)
  [ "$topic" -eq "$progress" ] && break # no new Topic videos: stop
done
echo "phase2: fetches=$fetches label_music=$label audio_track=$topic skipped=$skipped" >&2

jq -s 'sort_by(.category, .video_id)' "$TMP/kept.jsonl" > "$OUT"
echo "label_music=$label audio_track=$topic skipped=$skipped -> $OUT" >&2
full
