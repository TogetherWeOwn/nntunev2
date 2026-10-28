import { describe, expect, it } from 'vitest';

import type { LibraryItem } from './cache.js';
import * as orderModule from './order.js';

/**
 * Acceptance: find already-loaded library tracks by title or artist (pure half).
 *
 * OWNERSHIP: this file is acceptance-owned. The paired build lands the matcher
 * on top of this branch and must make these tests pass WITHOUT editing this
 * file; a wrong test comes back to the acceptance lane instead of being fixed
 * beside the implementation. The mocked-browser half is
 * `apps/web/tests/library-local-filter.spec.ts`.
 *
 * Contract the build must provide — a pure, presentation-only matcher:
 * - `filterLibraryItems(items, query)` exported from EITHER the new module
 *   `apps/web/src/lib/library/filter.ts` (preferred) OR as an added export of
 *   `apps/web/src/lib/library/order.ts`. The loader below tries `filter.js`
 *   first, then `order.js`, so the build picks one home without touching this
 *   file. A matcher hidden inside a route component cannot satisfy these
 *   independently executable cases; this codebase already factors pure library
 *   logic into lib modules (`order.ts`, `cache.ts`, `favorites.ts`), and the
 *   paired browser spec pins the wired UI separately.
 * - Signature: `(items: readonly LibraryItem[], query: unknown) => LibraryItem[]`.
 * - Match a TRIMMED literal case-insensitive substring over the existing safe
 *   display title (`displayTitle`: title when non-blank, else the stable id)
 *   and the optional display artist (`displayArtist`: null when missing or
 *   blank). Either field matching keeps the row (OR).
 * - Fold case with deterministic `String.toLowerCase()` — never
 *   `toLocaleLowerCase`, never `new RegExp(query)`. Regex metacharacters in
 *   the query are literal text. Do not strip accents, normalize Unicode, or
 *   trim the haystack; only the query is trimmed.
 * - An empty, whitespace-only or non-string query returns every row. Zero
 *   matches returns `[]`. Both are NEW arrays in input order holding the SAME
 *   object references (no clones); the input array and its items are never
 *   mutated. Equal matches keep input order — no secondary key, no resort.
 * - Titled rows are never matched by their id: the id is searched ONLY through
 *   the `displayTitle` fallback (missing/blank/invalid title).
 *
 * RED shape: every case loads the matcher through `requireFilter`, which turns
 * a missing export into an assertion failure (`expected undefined to be
 * defined`, naming both canonical pointers) instead of a collection-time
 * import error — so RED here always means the behavior is missing, never that
 * the harness cannot load. The `// @ts-ignore` on the `filter.js` dynamic
 * import is load-bearing for the same reason: it silences the missing-module
 * type error until the build adds the file, and `ts-ignore` (unlike
 * `ts-expect-error`) stays green afterwards without an edit to this file.
 *
 * Case IDs (stable): LF-U01 mixed-case title, LF-U02 trimmed query, LF-U03
 * literal metacharacters, LF-U04 invalid-title id fallback, LF-U05 invalid
 * artist, LF-U06 id fallback + titled-id exclusion, LF-U07 artist OR title,
 * LF-U08 empty/whitespace query, LF-U09 zero matches, LF-U10 stable ties +
 * immutability, LF-U11 non-string query, LF-U12 deterministic folding without
 * accent stripping, LF-U13 untrimmed haystack.
 *
 * Focused command (repo root, deterministic, offline):
 *   pnpm --dir apps/web exec vitest run src/lib/library/local-filter-acceptance.test.ts
 */

type FilterFn = (items: readonly LibraryItem[], query: unknown) => LibraryItem[];

async function loadFilter(): Promise<FilterFn | undefined> {
	try {
		// @ts-ignore: ./filter.js lands with the paired build; ts-ignore stays
		// green after it exists, unlike ts-expect-error (this file is frozen).
		const created = await import('./filter.js');
		const fn = (created as Record<string, unknown>)['filterLibraryItems'];
		if (typeof fn === 'function') return fn as FilterFn;
	} catch {
		// Missing module: fall through to the order.js extension option.
	}
	const extended = (orderModule as unknown as Record<string, unknown>)['filterLibraryItems'];
	return typeof extended === 'function' ? (extended as FilterFn) : undefined;
}

const MISSING =
	'missing behavior: implement filterLibraryItems(items, query) in ' +
	'apps/web/src/lib/library/filter.ts (new file) or as an export of ' +
	'apps/web/src/lib/library/order.ts — contract header of ' +
	'apps/web/src/lib/library/local-filter-acceptance.test.ts';

async function requireFilter(): Promise<FilterFn> {
	const fn = await loadFilter();
	expect(fn, MISSING).toBeDefined();
	return fn as FilterFn;
}

const ids = (items: readonly LibraryItem[]) => items.map((item) => item.id);

describe('find in loaded tracks: pure matching', () => {
	it('[LF-U01] matches a mixed-case substring over the display title', async () => {
		const filter = await requireFilter();
		const items = [
			{ id: 'u01-a', title: 'Bohemian Rhapsody', artist: 'Queen' },
			{ id: 'u01-b', title: 'Stairway to Heaven', artist: 'Led Zeppelin' }
		];
		expect(ids(filter(items, 'hemiAN'))).toEqual(['u01-a']);
		expect(ids(filter(items, 'STAIRWAY'))).toEqual(['u01-b']);
		expect(ids(filter(items, 'heaven'))).toEqual(['u01-b']);
	});

	it('[LF-U02] trims the query; whitespace-only behaves as empty', async () => {
		const filter = await requireFilter();
		const items = [
			{ id: 'u02-a', title: 'Bohemian Rhapsody' },
			{ id: 'u02-b', title: 'Stairway to Heaven' }
		];
		expect(ids(filter(items, '  stairway  '))).toEqual(['u02-b']);
		expect(ids(filter(items, '\t\nBohemian\n '))).toEqual(['u02-a']);
		expect(ids(filter(items, '   '))).toEqual(['u02-a', 'u02-b']);
	});

	it('[LF-U03] treats regex metacharacters as literal text, never as a pattern', async () => {
		const filter = await requireFilter();
		const items = [
			{ id: 'u03-star', title: 'What.*Ever' },
			{ id: 'u03-group', title: 'Pick (a|b) now' },
			{ id: 'u03-class', title: 'Find [abc] here' },
			{ id: 'u03-plus', title: 'C++ anthem', artist: 'A+B band' },
			{ id: 'u03-plain', title: 'Just words here' }
		];
		expect(ids(filter(items, '.*'))).toEqual(['u03-star']);
		expect(ids(filter(items, '(a|b)'))).toEqual(['u03-group']);
		expect(ids(filter(items, '[abc]'))).toEqual(['u03-class']);
		expect(ids(filter(items, 'C++'))).toEqual(['u03-plus']);
		expect(ids(filter(items, 'a+b'))).toEqual(['u03-plus']);
		// A user-generated regular expression would match here; a literal
		// matcher must not: no row contains the text 'a.c' or '^Just'.
		expect(ids(filter(items, 'a.c'))).toEqual([]);
		expect(ids(filter(items, '^Just'))).toEqual([]);
	});

	it('[LF-U04] falls back to the id for absent or invalid titles without throwing', async () => {
		const filter = await requireFilter();
		const items = [
			{ id: 'track-007' },
			{ id: 'u04-empty', title: '' },
			{ id: 'u04-blank', title: '   ' },
			{ id: 'u04-null', title: null },
			{ id: 'u04-num', title: 12 },
			{ id: 'u04-real', title: 'Real Title' }
		] as LibraryItem[];
		expect(ids(filter(items, 'track'))).toEqual(['track-007']);
		expect(ids(filter(items, 'u04-empty'))).toEqual(['u04-empty']);
		expect(ids(filter(items, 'U04-BLANK'))).toEqual(['u04-blank']);
		expect(ids(filter(items, 'u04-null'))).toEqual(['u04-null']);
		expect(ids(filter(items, 'u04-num'))).toEqual(['u04-num']);
		expect(ids(filter(items, 'real'))).toEqual(['u04-real']);
		expect(ids(filter(items, 'zzz-absent-everywhere'))).toEqual([]);
	});

	it('[LF-U05] ignores absent or invalid artists; title matching still applies', async () => {
		const filter = await requireFilter();
		const items = [
			{ id: 'u05-a', title: 'Song A' },
			{ id: 'u05-b', title: 'Song B', artist: '' },
			{ id: 'u05-c', title: 'Song C', artist: '   ' },
			{ id: 'u05-d', title: 'Song D', artist: null },
			{ id: 'u05-e', title: 'Song E', artist: 42 },
			{ id: 'u05-f', title: 'Love Tune', artist: 'Harbor Lights' }
		] as LibraryItem[];
		expect(ids(filter(items, 'song'))).toEqual(['u05-a', 'u05-b', 'u05-c', 'u05-d', 'u05-e']);
		expect(ids(filter(items, 'harbor'))).toEqual(['u05-f']);
		// The raw invalid value is never searchable: no id, title or display
		// artist contains '42'.
		expect(ids(filter(items, '42'))).toEqual([]);
	});

	it('[LF-U06] searches the id only through the title fallback, case-insensitively', async () => {
		const filter = await requireFilter();
		const items = [
			{ id: 'My-Track-42' },
			{ id: 'u06-titled', title: 'Some Title' }
		];
		expect(ids(filter(items, 'my-track'))).toEqual(['My-Track-42']);
		expect(ids(filter(items, 'TRACK-42'))).toEqual(['My-Track-42']);
		// The titled row keeps its title-only search: its own id must not match.
		expect(ids(filter(items, 'u06-titled'))).toEqual([]);
	});

	it('[LF-U07] keeps a row when the title OR the artist matches', async () => {
		const filter = await requireFilter();
		const items = [
			{ id: 'u07-a', title: 'Morning Song', artist: 'Harbor Lights' },
			{ id: 'u07-b', title: 'Harbor Song', artist: 'Other Band' },
			{ id: 'u07-c', title: 'Unrelated', artist: 'Nobody' }
		];
		expect(ids(filter(items, 'harbor'))).toEqual(['u07-a', 'u07-b']);
		expect(ids(filter(items, 'lights'))).toEqual(['u07-a']);
		expect(ids(filter(items, '  LIGHTS  '))).toEqual(['u07-a']);
	});

	it('[LF-U08] returns every row for an empty or whitespace-only query', async () => {
		const filter = await requireFilter();
		const items = [
			{ id: 'u08-b', title: 'Bravo' },
			{ id: 'u08-a', title: 'Alpha' }
		];
		for (const query of ['', '   ', '\t\n ', '   ']) {
			const result = filter(items, query);
			expect(ids(result), JSON.stringify(query)).toEqual(['u08-b', 'u08-a']);
			expect(result, JSON.stringify(query)).not.toBe(items);
			expect(result[0]).toBe(items[0]);
			expect(result[1]).toBe(items[1]);
		}
	});

	it('[LF-U09] returns a new empty array for zero matches and for empty input', async () => {
		const filter = await requireFilter();
		const items = [
			{ id: 'u09-a', title: 'Alpha' },
			{ id: 'u09-b', title: 'Beta' }
		];
		const result = filter(items, 'zzz-no-such-track');
		expect(result).toEqual([]);
		expect(result).not.toBe(items);
		expect(result.length).toBe(0);
		expect(filter([], 'song')).toEqual([]);
		expect(filter([], '')).toEqual([]);
	});

	it('[LF-U10] keeps input order for ties and never mutates frozen input', async () => {
		const filter = await requireFilter();
		const items = Object.freeze([
			Object.freeze({ id: 'u10-z', title: 'Same Song', artist: 'Zed' }),
			Object.freeze({ id: 'u10-a', title: 'same song', artist: 'Alpha' }),
			Object.freeze({ id: 'u10-m', title: 'SAME SONG', artist: 'Mike' }),
			Object.freeze({ id: 'u10-other', title: 'Different', artist: 'Zulu' })
		]);
		const before = JSON.stringify(items);
		const result = filter(items, 'same song');
		// All three ties match; the result follows the input, never a sort.
		expect(ids(result)).toEqual(['u10-z', 'u10-a', 'u10-m']);
		expect(result).not.toBe(items);
		expect(result[0]).toBe(items[0]);
		expect(result[1]).toBe(items[1]);
		expect(result[2]).toBe(items[2]);
		expect(JSON.stringify(items)).toBe(before);
		expect(ids(filter(items, ''))).toEqual(['u10-z', 'u10-a', 'u10-m', 'u10-other']);
	});

	it('[LF-U11] treats a non-string query as empty instead of throwing', async () => {
		const filter = await requireFilter();
		const items = [
			{ id: 'u11-a', title: 'Alpha' },
			{ id: 'u11-b', title: 'Beta' }
		];
		for (const query of [null, undefined, 42, true, {}, []]) {
			let result: LibraryItem[] | undefined;
			expect(() => {
				result = filter(items, query);
			}, `query ${JSON.stringify(query)} must not throw`).not.toThrow();
			expect(ids(result as LibraryItem[])).toEqual(['u11-a', 'u11-b']);
		}
	});

	it('[LF-U12] folds case deterministically without stripping accents', async () => {
		const filter = await requireFilter();
		const items = [
			{ id: 'u12-composed-upper', title: 'Éclair' },
			{ id: 'u12-composed-lower', title: 'éclair' },
			{ id: 'u12-ascii', title: 'Eclair' }
		];
		expect(ids(filter(items, 'éclair'))).toEqual(['u12-composed-upper', 'u12-composed-lower']);
		expect(ids(filter(items, 'ÉCLAIR'))).toEqual(['u12-composed-upper', 'u12-composed-lower']);
		// No accent stripping: the ASCII query matches only the ASCII title.
		expect(ids(filter(items, 'eclair'))).toEqual(['u12-ascii']);
		expect(ids(filter(items, 'ECLAIR'))).toEqual(['u12-ascii']);
	});

	it('[LF-U13] matches inside untrimmed display metadata', async () => {
		const filter = await requireFilter();
		const items = [{ id: 'u13-a', title: '  Padded Song  ' }];
		expect(ids(filter(items, 'padded'))).toEqual(['u13-a']);
		expect(ids(filter(items, 'padded song'))).toEqual(['u13-a']);
		expect(ids(filter(items, 'song'))).toEqual(['u13-a']);
	});
});
