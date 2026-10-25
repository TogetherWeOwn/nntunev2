import type { LibraryItem } from './cache.js';
import { displayArtist, displayTitle } from './order.js';

/**
 * Presentation-only match over already-loaded library rows: a trimmed literal
 * case-insensitive substring over the safe display title (title when
 * non-blank, else the stable id) and the optional display artist. Either
 * field matching keeps the row (OR).
 *
 * Case folds with deterministic `String.toLowerCase()` — never a
 * user-generated regular expression, so metacharacters are literal text and
 * accents are never stripped. Only the query is trimmed, never the haystack.
 * An empty, whitespace-only or non-string query returns every row; zero
 * matches returns `[]`. Both are NEW arrays in input order holding the SAME
 * object references; the input array and its items are never mutated. Equal
 * matches keep input order — no secondary key, no resort. Titled rows are
 * never matched by id: the id is searched ONLY through the title fallback.
 */
export function filterLibraryItems(
	items: readonly LibraryItem[],
	query: unknown
): LibraryItem[] {
	if (typeof query !== 'string') return [...items];
	const needle = query.trim().toLowerCase();
	if (needle.length === 0) return [...items];
	return items.filter((item) => {
		if (displayTitle(item).toLowerCase().includes(needle)) return true;
		const artist = displayArtist(item);
		return artist !== null && artist.toLowerCase().includes(needle);
	});
}
