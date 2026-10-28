import type { LibraryItem } from './cache.js';
import { canonicalMetadata, findNeedle, literalId, type FindNeedle } from './canonical-compare.js';
import { displayArtist } from './order.js';

/**
 * Presentation-only match over already-loaded library rows: a trimmed literal
 * case-insensitive substring over the safe display title (title when
 * non-blank, else the stable id) and the optional display artist. Either
 * field matching keeps the row (OR).
 *
 * Case folds with deterministic `String.toLowerCase()` and compares title and
 * artist metadata after NFC canonical normalization (see
 * `canonical-compare.ts`), so composed and decomposed spellings match in both
 * directions. Never a user-generated regular expression, so metacharacters are
 * literal text; accents are never stripped and compatibility forms never fold.
 * The stable-id title fallback stays byte-literal (case fold only). Only the
 * query is trimmed, never the haystack.
 * An empty, whitespace-only or non-string query returns every row; zero
 * matches returns `[]`. Both are NEW arrays in input order holding the SAME
 * object references; the input array and its items are never mutated. Equal
 * matches keep input order — no secondary key, no resort. Titled rows are
 * never matched by id: the id is searched ONLY through the title fallback.
 */
/** Which display fields a find searches. `'both'` is the shipped OR. */
export type FindScope = 'both' | 'title' | 'artist';

/**
 * Resolve any external scope input to a valid scope. Only the exact tokens
 * `'title'` and `'artist'` narrow the find; anything else (missing, unknown,
 * non-string) resolves to `'both'` so bad input can never narrow by accident.
 */
export function resolveFindScope(scope: unknown): FindScope {
	return scope === 'title' || scope === 'artist' ? scope : 'both';
}

function titleMatches(item: LibraryItem, needle: FindNeedle): boolean {
	const title = item.title;
	return typeof title === 'string' && title.trim().length > 0
		? canonicalMetadata(title).includes(needle.canonical)
		: literalId(item.id).includes(needle.literal);
}

export function filterLibraryItems(
	items: readonly LibraryItem[],
	query: unknown,
	scope?: unknown
): LibraryItem[] {
	if (typeof query !== 'string') return [...items];
	const needle = findNeedle(query);
	if (needle.literal.length === 0) return [...items];
	const resolved = resolveFindScope(scope);
	return items.filter((item) => {
		if (resolved !== 'artist' && titleMatches(item, needle)) return true;
		if (resolved === 'title') return false;
		const artist = displayArtist(item);
		return artist !== null && canonicalMetadata(artist).includes(needle.canonical);
	});
}
