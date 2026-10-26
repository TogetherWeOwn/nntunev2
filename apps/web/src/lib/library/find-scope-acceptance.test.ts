import { describe, expect, it } from 'vitest';

import type { LibraryItem } from './cache.js';

/**
 * Acceptance: narrow the loaded-library find to title or artist (pure half).
 *
 * OWNERSHIP: this file is acceptance-owned. The paired build extends the
 * matcher on top of this branch and must make these tests pass WITHOUT
 * editing this file; a wrong test comes back to the acceptance lane instead
 * of being fixed beside the implementation. The mocked-browser half is
 * `apps/web/tests/library-find-scope.spec.ts`. The pre-existing bank
 * `local-filter-acceptance.test.ts` stays protected and unedited: every
 * behavior it pins for the two-argument call must keep passing.
 *
 * Contract the build must provide — a compatible extension of the shipped
 * pure matcher in `apps/web/src/lib/library/filter.ts`:
 * - `filterLibraryItems(items, query, scope?)` gains an OPTIONAL third
 *   argument. The two-argument call is unchanged in every observable way.
 * - Valid scope tokens are exactly `'both'`, `'title'` and `'artist'`
 *   (the UI labels are "Both title and artist" / "Title only" /
 *   "Artist only"; the tokens are the machine form). `'both'` is the
 *   shipped OR over the safe display title (`displayTitle`: title when
 *   non-blank, else the stable id) and the optional display artist
 *   (`displayArtist`: null when missing or blank). `'title'` searches the
 *   display title ONLY, so the stable-id fallback stays searchable.
 *   `'artist'` searches the display artist ONLY: a row with a missing,
 *   blank or invalid artist can never match a non-empty query, and the
 *   id-as-title fallback is never searched.
 * - ANY other scope value (null, empty string, unknown token, non-string)
 *   resolves safely to `'both'` without throwing. External input can
 *   therefore never narrow a find by accident.
 * - Everything the shipped matcher already guarantees is preserved under
 *   every scope: only the query is trimmed, deterministic
 *   `String.toLowerCase()` folding, no user-generated regular expression
 *   (metacharacters are literal text), no accent stripping or Unicode
 *   normalization redesign, an empty/whitespace-only/non-string query
 *   returns every row, zero matches returns a new `[]`, results are NEW
 *   arrays in input order holding the SAME object references, and the
 *   input array and its items are never mutated.
 * - No query syntax: `artist:x` / `title:x` style text is a literal
 *   needle under every scope.
 * - If a canonical-equivalence matcher has landed by the time this builds,
 *   extend THAT matcher's per-field test instead of re-implementing field
 *   tests here; the scope only chooses which fields are tested. At the
 *   branch baseline the shipped matcher strips no accents, and FS-U08
 *   pins that shipped policy under scope.
 *
 * RED shape: the loader below resolves the shipped matcher and asserts it
 * exists, so RED always means the scoped behavior is missing (the
 * two-argument baseline ignores the third argument), never that the
 * harness cannot load. The FS-C controls call with two arguments only and
 * must pass before AND after the build.
 *
 * Case IDs (stable): FS-C01/FS-C02 baseline controls, FS-U01 cross-field
 * collisions, FS-U02 symmetric phrase split across fields, FS-U03
 * missing/blank/invalid artists, FS-U04 stable-id title fallback per
 * scope, FS-U05 empty/non-string query under scope, FS-U06 invalid scope
 * resolves to both, FS-U07 literal metacharacters and literal query
 * syntax, FS-U08 case/trim/accent policy under scope, FS-U09 stable
 * order/references/immutability, FS-U10 explicit both equals the
 * two-argument call, FS-U11 zero matches and empty input under scope.
 *
 * Focused command (repo root, deterministic, offline):
 *   pnpm --dir apps/web exec vitest run src/lib/library/find-scope-acceptance.test.ts
 */

type FindScope = 'both' | 'title' | 'artist';
type FilterFn = (
	items: readonly LibraryItem[],
	query: unknown,
	scope?: unknown
) => LibraryItem[];

async function loadFilter(): Promise<FilterFn | undefined> {
	try {
		const created = await import('./filter.js');
		const fn = (created as Record<string, unknown>)['filterLibraryItems'];
		if (typeof fn === 'function') return fn as FilterFn;
	} catch {
		// Missing module: fall through to the missing-behavior assertion.
	}
	return undefined;
}

const MISSING =
	'missing behavior: extend filterLibraryItems(items, query, scope?) in ' +
	'apps/web/src/lib/library/filter.ts with scope tokens both/title/artist ' +
	'(invalid scope resolves to both) — contract header of ' +
	'apps/web/src/lib/library/find-scope-acceptance.test.ts';

async function requireFilter(): Promise<FilterFn> {
	const fn = await loadFilter();
	expect(fn, MISSING).toBeDefined();
	return fn as FilterFn;
}

const ids = (items: readonly LibraryItem[]) => items.map((item) => item.id);

// 'harbor' lives in a different field on each row: a has it in the title
// only, b in the artist only, c in both, d in neither.
const COLLISIONS: LibraryItem[] = [
	{ id: 'fs-a', title: 'Harbor Song', artist: 'Echo' },
	{ id: 'fs-b', title: 'Morning Song', artist: 'Harbor Lights' },
	{ id: 'fs-c', title: 'Harbor Lights', artist: 'Harbor' },
	{ id: 'fs-d', title: 'Unrelated Tune', artist: 'Nobody' }
];

describe('find scope: baseline controls (two-argument call, pass before and after)', () => {
	it('[FS-C01] the two-argument call keeps the shipped title-OR-artist match', async () => {
		const filter = await requireFilter();
		expect(ids(filter(COLLISIONS, 'harbor'))).toEqual(['fs-a', 'fs-b', 'fs-c']);
		expect(ids(filter(COLLISIONS, '  HARBOR  '))).toEqual(['fs-a', 'fs-b', 'fs-c']);
		expect(ids(filter(COLLISIONS, 'echo'))).toEqual(['fs-a']);
		expect(ids(filter(COLLISIONS, 'unrelated'))).toEqual(['fs-d']);
		expect(ids(filter(COLLISIONS, 'zzz-absent-everywhere'))).toEqual([]);
	});

	it('[FS-C02] the two-argument call keeps empty-query, literal and id-fallback behavior', async () => {
		const filter = await requireFilter();
		const items: LibraryItem[] = [
			{ id: 'fs-ctl-untitled' },
			{ id: 'fs-ctl-regex', title: 'What.*Ever', artist: 'C++ Players' },
			{ id: 'fs-ctl-titled', title: 'Real Title', artist: 'Real Artist' }
		];
		expect(ids(filter(items, ''))).toEqual(['fs-ctl-untitled', 'fs-ctl-regex', 'fs-ctl-titled']);
		expect(ids(filter(items, '.*'))).toEqual(['fs-ctl-regex']);
		expect(ids(filter(items, 'fs-ctl-untitled'))).toEqual(['fs-ctl-untitled']);
		// A titled row is never matched by its own id.
		expect(ids(filter(items, 'fs-ctl-titled'))).toEqual([]);
	});
});

describe('find scope: pure matching', () => {
	it('[FS-U01] title and artist scopes split the same word by the field it lives in', async () => {
		const filter = await requireFilter();
		expect(ids(filter(COLLISIONS, 'harbor', 'both'))).toEqual(['fs-a', 'fs-b', 'fs-c']);
		expect(ids(filter(COLLISIONS, 'harbor', 'title'))).toEqual(['fs-a', 'fs-c']);
		expect(ids(filter(COLLISIONS, 'harbor', 'artist'))).toEqual(['fs-b', 'fs-c']);
		// 'song' is in two titles and no artist.
		expect(ids(filter(COLLISIONS, 'song', 'both'))).toEqual(['fs-a', 'fs-b']);
		expect(ids(filter(COLLISIONS, 'song', 'title'))).toEqual(['fs-a', 'fs-b']);
		expect(ids(filter(COLLISIONS, 'song', 'artist'))).toEqual([]);
		// 'echo' is in one artist and no title.
		expect(ids(filter(COLLISIONS, 'echo', 'both'))).toEqual(['fs-a']);
		expect(ids(filter(COLLISIONS, 'echo', 'title'))).toEqual([]);
		expect(ids(filter(COLLISIONS, 'echo', 'artist'))).toEqual(['fs-a']);
	});

	it('[FS-U02] a phrase present as one title and one artist splits symmetrically', async () => {
		const filter = await requireFilter();
		// 'lights': fs-b via artist "Harbor Lights", fs-c via title "Harbor Lights".
		expect(ids(filter(COLLISIONS, 'lights', 'both'))).toEqual(['fs-b', 'fs-c']);
		expect(ids(filter(COLLISIONS, 'lights', 'title'))).toEqual(['fs-c']);
		expect(ids(filter(COLLISIONS, 'lights', 'artist'))).toEqual(['fs-b']);
		expect(ids(filter(COLLISIONS, ' harbor lights ', 'title'))).toEqual(['fs-c']);
		expect(ids(filter(COLLISIONS, ' harbor lights ', 'artist'))).toEqual(['fs-b']);
	});

	it('[FS-U03] artist scope excludes missing, blank and invalid artists for non-empty queries', async () => {
		const filter = await requireFilter();
		const items = [
			{ id: 'fs-noartist', title: 'Solo Title' },
			{ id: 'fs-blankartist', title: 'Quiet Title', artist: '   ' },
			{ id: 'fs-badartist', title: 'Odd Title', artist: 42 },
			{ id: 'fs-nullartist', title: 'Null Title', artist: null },
			{ id: 'fs-hasartist', title: 'Plain Title', artist: 'Solo Artist' }
		] as LibraryItem[];
		// 'solo' is a title on the no-artist row and the artist of the last row.
		expect(ids(filter(items, 'solo', 'both'))).toEqual(['fs-noartist', 'fs-hasartist']);
		expect(ids(filter(items, 'solo', 'title'))).toEqual(['fs-noartist']);
		expect(ids(filter(items, 'solo', 'artist'))).toEqual(['fs-hasartist']);
		// 'title' is in every title; no artist field contains it.
		expect(ids(filter(items, 'title', 'artist'))).toEqual([]);
		expect(ids(filter(items, 'title', 'title'))).toEqual([
			'fs-noartist',
			'fs-blankartist',
			'fs-badartist',
			'fs-nullartist',
			'fs-hasartist'
		]);
		// The raw invalid artist value is never searchable in any scope (no
		// id, title or display artist above contains '42').
		for (const scope of ['both', 'title', 'artist'] as const) {
			expect(ids(filter(items, '42', scope)), scope).toEqual([]);
		}
	});

	it('[FS-U04] the stable-id title fallback is searchable in title/both, never in artist', async () => {
		const filter = await requireFilter();
		const items: LibraryItem[] = [
			{ id: 'fs-untitled-one' },
			{ id: 'fs-blank-one', title: '   ', artist: 'Harbor Lights' },
			{ id: 'fs-titled-one', title: 'Real Title', artist: 'Real Artist' }
		];
		expect(ids(filter(items, 'fs-untitled', 'both'))).toEqual(['fs-untitled-one']);
		expect(ids(filter(items, 'fs-untitled', 'title'))).toEqual(['fs-untitled-one']);
		expect(ids(filter(items, 'fs-untitled', 'artist'))).toEqual([]);
		// A blank title falls back to the id even when an artist is present.
		expect(ids(filter(items, 'FS-BLANK', 'title'))).toEqual(['fs-blank-one']);
		expect(ids(filter(items, 'FS-BLANK', 'artist'))).toEqual([]);
		expect(ids(filter(items, 'harbor', 'artist'))).toEqual(['fs-blank-one']);
		// A titled row never matches by its own id, in any scope.
		for (const scope of ['both', 'title', 'artist'] as const) {
			expect(ids(filter(items, 'fs-titled-one', scope)), scope).toEqual([]);
		}
	});

	it('[FS-U05] an empty, whitespace-only or non-string query returns every row in every scope', async () => {
		const filter = await requireFilter();
		const items: LibraryItem[] = [
			{ id: 'fs-e1' },
			{ id: 'fs-e2', title: 'Bravo Only' },
			{ id: 'fs-e3', title: 'Alpha Only', artist: 'Someone' }
		];
		const all = ['fs-e1', 'fs-e2', 'fs-e3'];
		for (const scope of ['both', 'title', 'artist', 'album', null, undefined] as unknown[]) {
			for (const query of ['', '   ', '\t\n ', null, undefined, 42, true, {}, []]) {
				const result = filter(items, query, scope);
				expect(ids(result), `scope=${String(scope)} query=${String(query)}`).toEqual(all);
				expect(result, 'empty-query result must be a new array').not.toBe(items);
				expect(result[0]).toBe(items[0]);
			}
		}
		// In particular artist scope does NOT hide the no-artist rows when the
		// query is empty: scoping narrows matches, it is not a visibility filter.
		expect(ids(filter(items, '', 'artist'))).toEqual(all);
	});

	it('[FS-U06] any invalid scope resolves safely to both without throwing', async () => {
		const filter = await requireFilter();
		const expectedBoth = ['fs-a', 'fs-b', 'fs-c'];
		const invalidScopes: unknown[] = [
			undefined,
			null,
			'',
			'album',
			'genre',
			'track',
			0,
			42,
			true,
			false,
			{},
			[],
			['title']
		];
		for (const scope of invalidScopes) {
			let result: LibraryItem[] | undefined;
			expect(() => {
				result = filter(COLLISIONS, 'harbor', scope);
			}, `scope ${JSON.stringify(scope) ?? String(scope)} must not throw`).not.toThrow();
			expect(ids(result as LibraryItem[]), `scope=${String(scope)}`).toEqual(expectedBoth);
			// The cross-field split must NOT happen for a bad scope: 'echo' is
			// artist-only on fs-a, and both still finds it.
			expect(ids(filter(COLLISIONS, 'echo', scope)), `scope=${String(scope)}`).toEqual(['fs-a']);
		}
	});

	it('[FS-U07] metacharacters and field-prefix syntax stay literal under every scope', async () => {
		const filter = await requireFilter();
		const items: LibraryItem[] = [
			{ id: 'fs-lit', title: 'What.*Ever (a|b)', artist: 'C++ Players [live]' },
			{ id: 'fs-plain', title: 'Just Words', artist: 'Plain Band' }
		];
		expect(ids(filter(items, '.*', 'title'))).toEqual(['fs-lit']);
		expect(ids(filter(items, '.*', 'artist'))).toEqual([]);
		expect(ids(filter(items, '(a|b)', 'both'))).toEqual(['fs-lit']);
		expect(ids(filter(items, 'c++', 'artist'))).toEqual(['fs-lit']);
		expect(ids(filter(items, 'c++', 'title'))).toEqual([]);
		expect(ids(filter(items, '[live]', 'artist'))).toEqual(['fs-lit']);
		expect(ids(filter(items, '[live]', 'title'))).toEqual([]);
		// No query syntax: the prefix is literal text that no row contains.
		for (const scope of ['both', 'title', 'artist'] as const) {
			expect(ids(filter(items, 'artist:c++', scope)), scope).toEqual([]);
			expect(ids(filter(items, 'title:what', scope)), scope).toEqual([]);
			expect(ids(filter(items, 'track:just', scope)), scope).toEqual([]);
		}
	});

	it('[FS-U08] case folding, query trimming and no-accent-stripping hold per field', async () => {
		const filter = await requireFilter();
		const items: LibraryItem[] = [
			{ id: 'fs-accent', title: 'Éclair', artist: 'écho' },
			{ id: 'fs-ascii', title: 'Eclair', artist: 'Echo' },
			{ id: 'fs-padded', title: '  Padded Song  ', artist: '  Loose Band  ' }
		];
		expect(ids(filter(items, 'ÉCLAIR', 'title'))).toEqual(['fs-accent']);
		expect(ids(filter(items, 'eclair', 'title'))).toEqual(['fs-ascii']);
		expect(ids(filter(items, 'eclair', 'artist'))).toEqual([]);
		expect(ids(filter(items, 'ÉCHO', 'artist'))).toEqual(['fs-accent']);
		expect(ids(filter(items, 'echo', 'artist'))).toEqual(['fs-ascii']);
		expect(ids(filter(items, 'echo', 'title'))).toEqual([]);
		// Only the query is trimmed; the haystack keeps its padding.
		expect(ids(filter(items, '  padded song  ', 'title'))).toEqual(['fs-padded']);
		expect(ids(filter(items, '\tLoose\n', 'artist'))).toEqual(['fs-padded']);
	});

	it('[FS-U09] scoped results keep input order and references, and never mutate frozen input', async () => {
		const filter = await requireFilter();
		const items = Object.freeze([
			Object.freeze({ id: 'fs-o-z', title: 'Same Song', artist: 'Harbor' }),
			Object.freeze({ id: 'fs-o-a', title: 'Harbor Song', artist: 'Alpha' }),
			Object.freeze({ id: 'fs-o-m', title: 'Other Song', artist: 'Harbor' }),
			Object.freeze({ id: 'fs-o-x', title: 'Different', artist: 'Zulu' })
		]) as readonly LibraryItem[];
		const before = JSON.stringify(items);
		const byTitle = filter(items, 'harbor', 'title');
		expect(ids(byTitle)).toEqual(['fs-o-a']);
		const byArtist = filter(items, 'harbor', 'artist');
		// Input order (z before m), never a resort; same object references.
		expect(ids(byArtist)).toEqual(['fs-o-z', 'fs-o-m']);
		expect(byArtist[0]).toBe(items[0]);
		expect(byArtist[1]).toBe(items[2]);
		expect(byArtist).not.toBe(items);
		const byBoth = filter(items, 'song', 'both');
		expect(ids(byBoth)).toEqual(['fs-o-z', 'fs-o-a', 'fs-o-m']);
		expect(byBoth[0]).toBe(items[0]);
		expect(byBoth[1]).toBe(items[1]);
		expect(byBoth[2]).toBe(items[2]);
		expect(JSON.stringify(items)).toBe(before);
	});

	it('[FS-U10] explicit both is indistinguishable from the shipped two-argument call', async () => {
		const filter = await requireFilter();
		const items: LibraryItem[] = [
			...COLLISIONS,
			{ id: 'fs-x-untitled' },
			{ id: 'fs-x-regex', title: 'What.*Ever', artist: 'C++ Players' },
			{ id: 'fs-x-accent', title: 'Éclair', artist: 'écho' }
		];
		const queries: unknown[] = [
			'harbor',
			'HARBOR',
			' song ',
			'echo',
			'.*',
			'c++',
			'éclair',
			'eclair',
			'fs-x-untitled',
			'zzz-absent',
			'',
			'   ',
			null,
			42
		];
		for (const query of queries) {
			const shipped = filter(items, query);
			const explicit = filter(items, query, 'both' satisfies FindScope);
			expect(ids(explicit), `query=${String(query)}`).toEqual(ids(shipped));
			expect(explicit, 'each call returns its own array').not.toBe(shipped);
			for (let i = 0; i < shipped.length; i += 1) {
				expect(explicit[i]).toBe(shipped[i]);
			}
		}
	});

	it('[FS-U11] zero matches and empty input return a new empty array under every scope', async () => {
		const filter = await requireFilter();
		for (const scope of ['both', 'title', 'artist', 'album'] as unknown[]) {
			const result = filter(COLLISIONS, 'zzz-no-such-track', scope);
			expect(result, `scope=${String(scope)}`).toEqual([]);
			expect(result).not.toBe(COLLISIONS);
			expect(filter([], 'harbor', scope)).toEqual([]);
			expect(filter([], '', scope)).toEqual([]);
		}
		// A query that only one field could match is a zero-match for the other.
		expect(filter(COLLISIONS, 'unrelated', 'artist')).toEqual([]);
		expect(filter(COLLISIONS, 'nobody', 'title')).toEqual([]);
	});
});
