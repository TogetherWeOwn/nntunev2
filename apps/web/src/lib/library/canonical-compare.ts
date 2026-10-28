/**
 * The one canonical comparison helper behind every track find (library rows
 * and the shared queue matcher). It adds NFC canonical normalization to the
 * existing trim/lowercase rules so a composed and a decomposed spelling of the
 * same accented name compare equal.
 *
 * Deliberately narrow: NFC only (never NFKC/NFKD), no mark stripping, no
 * transliteration, no fuzzy matching. `e` still never matches `é`, and
 * compatibility-only forms (ligatures, circled digits, fullwidth) stay
 * distinct. Comparison strings only — the originals are never rewritten.
 */

/**
 * Query side. `literal` (trimmed, lowercased, bytes untouched) is compared
 * against stable ids; `canonical` (the same plus NFC) against title/artist
 * metadata. An empty query is empty in both forms.
 */
export interface FindNeedle {
	readonly literal: string;
	readonly canonical: string;
}

export function findNeedle(query: string): FindNeedle {
	const literal = query.trim().toLowerCase();
	return { literal, canonical: literal.normalize('NFC') };
}

/** Title/artist metadata side: lowercased, NFC. Never trimmed. */
export function canonicalMetadata(value: string): string {
	return value.toLowerCase().normalize('NFC');
}

/**
 * Stable-id fallback side: byte-literal except for the existing case fold.
 * Ids are identifiers, not metadata, so canonical equivalence never applies;
 * pair it with `FindNeedle.literal`.
 */
export function literalId(id: string): string {
	return id.toLowerCase();
}
