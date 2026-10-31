import { describe, expect, it } from 'vitest';

import type { LibraryItem } from './cache.js';

/**
 * Acceptance: find loaded library tracks across canonically equivalent
 * Unicode spellings (pure half).
 *
 * OWNERSHIP: this file is acceptance-owned. The paired build lands the
 * canonical matcher on top of this branch and must make these tests pass
 * WITHOUT editing this file; a wrong test comes back to the acceptance
 * lane instead of being fixed beside the implementation. The
 * mocked-browser half is `apps/web/tests/canonical-track-find.spec.ts`
 * (library find, library queue and demo queue surfaces); the shared queue
 * matcher's pure half is
 * `apps/web/src/lib/player/canonical-queue-find-acceptance.test.ts`. The
 * pre-existing banks `local-filter-acceptance.test.ts` and
 * `find-scope-acceptance.test.ts` stay protected and unedited: every
 * behavior they pin must keep passing.
 *
 * Contract the build must provide — a compatible extension of the shipped
 * pure matcher `filterLibraryItems(items, query, scope?)` in
 * `apps/web/src/lib/library/filter.ts`:
 * - Compare with ONE canonical helper applying the existing trim/lowercase
 *   rules plus NFC canonical normalization to the comparison strings on
 *   BOTH sides (stored metadata and query). Canonically equivalent title
 *   or artist spellings match in BOTH query directions.
 * - Accents stay significant: `e` never matches `é`. No NFKC/NFKD, no
 *   mark stripping, no fuzzy matching, no transliteration, no user regex.
 *   Compatibility-only equivalents (fi ligature, circled digits,
 *   fullwidth forms, roman numerals) never match their ASCII lookalikes.
 * - Canonical-equivalence expansion applies to title/artist METADATA
 *   only. The stable-id title fallback stays byte-literal except for its
 *   existing case-fold/trim policy: an id containing a decomposed
 *   sequence never matches its composed spelling and vice versa. Titled
 *   rows are still never matched by id.
 * - Everything else the shipped matcher guarantees is preserved: OR over
 *   the safe display title and the optional display artist, scope tokens
 *   both/title/artist with invalid scope resolving to both, only the
 *   query trimmed, metacharacters literal, empty/whitespace/non-string
 *   query returns every row, zero matches returns a new `[]`, results are
 *   NEW arrays in input order holding the SAME object references, and the
 *   input array and its items are never mutated. Stored metadata, ids and
 *   the original query text are never rewritten.
 *
 * RED shape: the loader below resolves the shipped matcher and asserts it
 * exists, so RED always means the canonical behavior is missing (the
 * baseline compares byte-literally), never that the harness cannot load.
 * CF-C controls use ASCII/canonical-stable vectors and must pass before
 * AND after the build.
 *
 * Case IDs (stable): CF-C00 fixture byte-shape guard, CF-C01/CF-C02
 * baseline controls, CF-L01 composed title both directions, CF-L02
 * decomposed stored title, CF-L03 artist both directions, CF-L04 combined
 * marks, CF-L05 Hangul, CF-L06 accent-negative, CF-L07
 * compatibility-negative, CF-L08 id fallback byte-literal, CF-L09 id
 * case-fold/trim preserved, CF-L10 cross-field OR, CF-L11 scope
 * interplay, CF-L12 literals with canonical content, CF-L13 empty/non-string
 * queries, CF-L14 order/references/immutability.
 *
 * Focused command (repo root, deterministic, offline):
 *   pnpm --dir apps/web exec vitest run src/lib/library/canonical-find-acceptance.test.ts
 */

// Canonical pairs. Each constant's code points are pinned numerically by
// [CF-C00], so an editor normalizing this file fails loudly there instead
// of silently weakening the matching cases. Outside these constants the
// file is pure ASCII.
//   E_ACUTE = LATIN SMALL LETTER E WITH ACUTE (U+00E9, NFC)
//   E_DECOMPOSED = e + COMBINING ACUTE ACCENT (U+0301, NFD)
//   E_ACUTE_UPPER = LATIN CAPITAL LETTER E WITH ACUTE (U+00C9)
//   A_RING = LATIN CAPITAL LETTER A WITH RING ABOVE (U+00C5, NFC)
//   A_RING_DECOMPOSED = A + COMBINING RING ABOVE (U+030A, NFD)
//   A_CIRCUMFLEX_ACUTE = LATIN SMALL LETTER A WITH CIRCUMFLEX AND ACUTE (U+1EA5, NFC)
//   A_CIRCUMFLEX_ACUTE_DECOMPOSED = a + U+0302 + U+0301 (NFD)
//   HANGUL_GA = HANGUL SYLLABLE GA (U+AC00, NFC)
//   HANGUL_GA_DECOMPOSED = HANGUL CHOSEONG KIYEOK + HANGUL JUNGSEONG A (U+1100 U+1161, NFD)
//   LIGATURE_FI = LATIN SMALL LIGATURE FI (U+FB01, compatibility only)
//   CIRCLED_ONE = CIRCLED DIGIT ONE (U+2460, compatibility only)
//   FULLWIDTH_A = FULLWIDTH LATIN CAPITAL LETTER A (U+FF21, compatibility only)
//   ROMAN_EIGHT = ROMAN NUMERAL EIGHT (U+2167, compatibility only)
const E_ACUTE = 'é';
const E_DECOMPOSED = 'é';
const E_ACUTE_UPPER = 'É';
const A_RING = 'Å';
const A_RING_DECOMPOSED = 'Å';
const A_CIRCUMFLEX_ACUTE = 'ấ';
const A_CIRCUMFLEX_ACUTE_DECOMPOSED = 'ấ';
const HANGUL_GA = '가';
const HANGUL_GA_DECOMPOSED = '가';
const LIGATURE_FI = 'ﬁ';
const CIRCLED_ONE = '①';
const FULLWIDTH_A = 'Ａ';
const ROMAN_EIGHT = 'Ⅷ';

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
	'apps/web/src/lib/library/filter.ts with NFC canonical equivalence on ' +
	'title/artist comparison (ids stay byte-literal) — contract header of ' +
	'apps/web/src/lib/library/canonical-find-acceptance.test.ts';

async function requireFilter(): Promise<FilterFn> {
	const fn = await loadFilter();
	expect(fn, MISSING).toBeDefined();
	return fn as FilterFn;
}

const ids = (items: readonly LibraryItem[]) => items.map((item) => item.id);

const points = (value: string): (number | undefined)[] =>
	[...value].map((character) => character.codePointAt(0));

function receipt(case_id: string, expected: unknown, actual: unknown, status: 'PASS' | 'FAIL') {
	console.log(
		JSON.stringify({
			case_id,
			expected,
			actual,
			status,
			error_code: status === 'PASS' ? null : 'CANONICAL_FIND_CONTRACT',
			next_action:
				status === 'PASS'
					? 'none'
					: 'Apply NFC canonical equivalence to title/artist matching; keep ids byte-literal.'
		})
	);
}

async function check(case_id: string, expected: unknown, run: () => unknown) {
	let actual: unknown = { error: 'case did not run' };
	let ran = false;
	try {
		actual = run();
		ran = true;
		expect(actual).toEqual(expected);
		receipt(case_id, expected, actual, 'PASS');
	} catch (error) {
		if (!ran) actual = { error: error instanceof Error ? error.message : String(error) };
		receipt(case_id, expected, actual, 'FAIL');
		throw error;
	}
}

describe('canonical find: fixture guard and baseline controls (pass before and after)', () => {
	it('[CF-C00] fixture constants hold their intended code points and NFC relations', () => {
		expect(points(E_ACUTE)).toEqual([0xe9]);
		expect(points(E_DECOMPOSED)).toEqual([0x65, 0x301]);
		expect(points(E_ACUTE_UPPER)).toEqual([0xc9]);
		expect(points(A_RING)).toEqual([0xc5]);
		expect(points(A_RING_DECOMPOSED)).toEqual([0x41, 0x30a]);
		expect(points(A_CIRCUMFLEX_ACUTE)).toEqual([0x1ea5]);
		expect(points(A_CIRCUMFLEX_ACUTE_DECOMPOSED)).toEqual([0x61, 0x302, 0x301]);
		expect(points(HANGUL_GA)).toEqual([0xac00]);
		expect(points(HANGUL_GA_DECOMPOSED)).toEqual([0x1100, 0x1161]);
		expect(points(LIGATURE_FI)).toEqual([0xfb01]);
		expect(points(CIRCLED_ONE)).toEqual([0x2460]);
		expect(points(FULLWIDTH_A)).toEqual([0xff21]);
		expect(points(ROMAN_EIGHT)).toEqual([0x2167]);
		// The engine's NFC agrees with the intended equivalences, and the
		// compatibility-only scalars are NFC-stable (no NFKC involved).
		expect(E_DECOMPOSED.normalize('NFC')).toBe(E_ACUTE);
		expect(A_RING_DECOMPOSED.normalize('NFC')).toBe(A_RING);
		expect(A_CIRCUMFLEX_ACUTE_DECOMPOSED.normalize('NFC')).toBe(A_CIRCUMFLEX_ACUTE);
		expect(HANGUL_GA_DECOMPOSED.normalize('NFC')).toBe(HANGUL_GA);
		for (const stable of [E_ACUTE, LIGATURE_FI, CIRCLED_ONE, FULLWIDTH_A, ROMAN_EIGHT]) {
			expect(stable.normalize('NFC')).toBe(stable);
		}
		receipt('[CF-C00]', 'fixture byte shapes intact', 'fixture byte shapes intact', 'PASS');
	});

	it('[CF-C01] the shipped literal match is unchanged for ASCII metadata', async () => {
		const filter = await requireFilter();
		const items: LibraryItem[] = [
			{ id: 'cf-ascii-a', title: 'Harbor Song', artist: 'Echo Band' },
			{ id: 'cf-ascii-b', title: 'Morning Tune', artist: 'Harbor Lights' }
		];
		await check('[CF-C01]', ['cf-ascii-a', 'cf-ascii-b'], () => ids(filter(items, 'harbor')));
	});

	it('[CF-C02] composed-only accented metadata keeps its shipped behavior', async () => {
		const filter = await requireFilter();
		const items: LibraryItem[] = [
			{ id: 'cf-composed', title: `${E_ACUTE}clair`, artist: `${E_ACUTE}cho` },
			{ id: 'cf-plain', title: 'Eclair', artist: 'Echo' }
		];
		await check('[CF-C02]', ['cf-composed'], () => ids(filter(items, `${E_ACUTE}clair`)));
	});
});

describe('canonical find: pure matching over loaded library tracks', () => {
	it('[CF-L01] a composed title matches a decomposed query and vice versa', async () => {
		const filter = await requireFilter();
		const items: LibraryItem[] = [
			{ id: 'cf-title-composed', title: `${E_ACUTE}clair` },
			{ id: 'cf-title-other', title: 'Unrelated Tune' }
		];
		await check('[CF-L01a]', ['cf-title-composed'], () =>
			ids(filter(items, `${E_DECOMPOSED}clair`))
		);
		await check('[CF-L01b]', ['cf-title-composed'], () =>
			ids(filter(items, ` ${E_ACUTE_UPPER}CLAIR `))
		);
	});

	it('[CF-L02] a decomposed stored title matches a composed query', async () => {
		const filter = await requireFilter();
		const items: LibraryItem[] = [
			{ id: 'cf-title-decomposed', title: `${E_DECOMPOSED}clair` },
			{ id: 'cf-title-other', title: 'Unrelated Tune' }
		];
		await check('[CF-L02]', ['cf-title-decomposed'], () => ids(filter(items, `${E_ACUTE}clair`)));
	});

	it('[CF-L03] artist matching is canonically equivalent in both directions', async () => {
		const filter = await requireFilter();
		const items: LibraryItem[] = [
			{ id: 'cf-artist-composed', title: 'Northern Song', artist: `${A_RING}ngstrom` },
			{ id: 'cf-artist-decomposed', title: 'Southern Song', artist: `${A_RING_DECOMPOSED}ngstrom` },
			{ id: 'cf-artist-other', title: 'Other Song', artist: 'Nobody' }
		];
		await check('[CF-L03a]', ['cf-artist-composed', 'cf-artist-decomposed'], () =>
			ids(filter(items, `${A_RING}ngstrom`))
		);
		await check('[CF-L03b]', ['cf-artist-composed', 'cf-artist-decomposed'], () =>
			ids(filter(items, ` ${A_RING_DECOMPOSED}ngstrom `))
		);
	});

	it('[CF-L04] combined marks match their precomposed equivalent', async () => {
		const filter = await requireFilter();
		const items: LibraryItem[] = [
			{ id: 'cf-combined-composed', title: `S${A_CIRCUMFLEX_ACUTE}o Paulo Nights` },
			{ id: 'cf-combined-decomposed', title: `S${A_CIRCUMFLEX_ACUTE_DECOMPOSED}o Paulo Days` },
			{ id: 'cf-combined-other', title: 'Unrelated Tune' }
		];
		await check('[CF-L04a]', ['cf-combined-composed', 'cf-combined-decomposed'], () =>
			ids(filter(items, `s${A_CIRCUMFLEX_ACUTE}o`))
		);
		await check('[CF-L04b]', ['cf-combined-composed', 'cf-combined-decomposed'], () =>
			ids(filter(items, `S${A_CIRCUMFLEX_ACUTE_DECOMPOSED}o`))
		);
	});

	it('[CF-L05] Hangul syllables match their conjoining jamo', async () => {
		const filter = await requireFilter();
		const items: LibraryItem[] = [
			{ id: 'cf-hangul-composed', title: `${HANGUL_GA} Morning` },
			{ id: 'cf-hangul-decomposed', title: `${HANGUL_GA_DECOMPOSED} Evening` },
			{ id: 'cf-hangul-other', title: 'Unrelated Tune' }
		];
		await check('[CF-L05a]', ['cf-hangul-composed', 'cf-hangul-decomposed'], () =>
			ids(filter(items, HANGUL_GA))
		);
		await check('[CF-L05b]', ['cf-hangul-composed', 'cf-hangul-decomposed'], () =>
			ids(filter(items, ` ${HANGUL_GA_DECOMPOSED} `))
		);
	});

	it('[CF-L06] accents stay significant: bare e never matches accented vowels', async () => {
		const filter = await requireFilter();
		const items: LibraryItem[] = [
			{ id: 'cf-accented', title: `${E_ACUTE}clair`, artist: `${E_ACUTE}cho` },
			{ id: 'cf-bare', title: 'Eclair', artist: 'Echo' }
		];
		await check('[CF-L06a]', ['cf-bare'], () => ids(filter(items, 'eclair')));
		await check('[CF-L06b]', ['cf-bare'], () => ids(filter(items, 'ECLAIR')));
		await check('[CF-L06c]', ['cf-accented'], () => ids(filter(items, `${E_ACUTE}clair`)));
		await check('[CF-L06d]', ['cf-bare'], () => ids(filter(items, 'echo')));
	});

	it('[CF-L07] compatibility-only lookalikes never match', async () => {
		const filter = await requireFilter();
		const items: LibraryItem[] = [
			{ id: 'cf-compat-fi', title: `${LIGATURE_FI}nishing Song` },
			{ id: 'cf-compat-circled', title: `Take ${CIRCLED_ONE}` },
			{ id: 'cf-compat-fullwidth', title: `${FULLWIDTH_A}pple Song` },
			{ id: 'cf-compat-roman', title: `Part ${ROMAN_EIGHT}` },
			{ id: 'cf-compat-ascii', title: 'Finishing Take Apple Part' }
		];
		// Neither direction may match: no NFKC/NFKD, no transliteration.
		await check('[CF-L07a]', ['cf-compat-ascii'], () => ids(filter(items, 'finishing')));
		await check('[CF-L07b]', ['cf-compat-fi'], () => ids(filter(items, `${LIGATURE_FI}nishing`)));
		await check('[CF-L07c]', [], () => ids(filter(items, 'take 1')));
		await check('[CF-L07d]', ['cf-compat-circled'], () => ids(filter(items, `take ${CIRCLED_ONE}`)));
		await check('[CF-L07e]', ['cf-compat-ascii'], () => ids(filter(items, 'apple')));
		await check('[CF-L07f]', ['cf-compat-fullwidth'], () => ids(filter(items, `${FULLWIDTH_A}pple`)));
		await check('[CF-L07g]', [], () => ids(filter(items, 'part viii')));
		await check('[CF-L07h]', ['cf-compat-roman'], () => ids(filter(items, `part ${ROMAN_EIGHT}`)));
	});

	it('[CF-L08] the stable-id fallback stays byte-literal in both directions', async () => {
		const filter = await requireFilter();
		const decomposedId: LibraryItem[] = [{ id: `canon-${E_DECOMPOSED}-id` }];
		const composedId: LibraryItem[] = [{ id: `canon-${E_ACUTE}-id`, title: '   ' }];
		// Byte-literal ids: a canonically equivalent but byte-different
		// spelling must NOT match through the fallback.
		await check('[CF-L08a]', [], () => ids(filter(decomposedId, `canon-${E_ACUTE}-id`)));
		await check('[CF-L08b]', [], () => ids(filter(composedId, `canon-${E_DECOMPOSED}-id`)));
		// Exact (case-insensitive) id text still matches through the fallback.
		await check('[CF-L08c]', [`canon-${E_DECOMPOSED}-id`], () =>
			ids(filter(decomposedId, `CANON-${E_DECOMPOSED}-ID`))
		);
		await check('[CF-L08d]', [`canon-${E_ACUTE}-id`], () =>
			ids(filter(composedId, `canon-${E_ACUTE}-id `.trim()))
		);
	});

	it('[CF-L09] titled rows are still never matched by id', async () => {
		const filter = await requireFilter();
		const items: LibraryItem[] = [
			{ id: 'cf-titled-row', title: `${E_ACUTE}clair Song` },
			{ id: 'cf-untitled-row' }
		];
		await check('[CF-L09a]', [], () => ids(filter(items, 'cf-titled-row')));
		await check('[CF-L09b]', ['cf-untitled-row'], () => ids(filter(items, 'CF-UNTITLED-ROW')));
		// The titled row still matches by its metadata (canonical title
		// directions are pinned by CF-L01/CF-L02/CF-L10 instead).
		await check('[CF-L09c]', ['cf-titled-row'], () => ids(filter(items, `${E_ACUTE}clair`)));
	});

	it('[CF-L10] a canonical match in either field keeps the row (OR)', async () => {
		const filter = await requireFilter();
		const items: LibraryItem[] = [
			{ id: 'cf-or-title', title: `${E_ACUTE}clair Song`, artist: 'Nobody' },
			{ id: 'cf-or-artist', title: 'Plain Title', artist: `Harbor ${E_ACUTE}cho` },
			{ id: 'cf-or-neither', title: 'Plain Title', artist: 'Nobody' }
		];
		await check('[CF-L10a]', ['cf-or-title'], () => ids(filter(items, `${E_DECOMPOSED}clair`)));
		await check('[CF-L10b]', ['cf-or-artist'], () =>
			ids(filter(items, `harbor ${E_DECOMPOSED}cho`))
		);
	});

	it('[CF-L11] canonical matching composes with the title/artist scopes', async () => {
		const filter = await requireFilter();
		const items: LibraryItem[] = [
			{ id: 'cf-scope-title', title: `${E_ACUTE}clair Song`, artist: 'Nobody' },
			{ id: 'cf-scope-artist', title: 'Plain Title', artist: `${E_ACUTE}cho Band` }
		];
		await check('[CF-L11a]', ['cf-scope-title'], () =>
			ids(filter(items, `${E_DECOMPOSED}clair`, 'title'))
		);
		await check('[CF-L11b]', [], () => ids(filter(items, `${E_DECOMPOSED}clair`, 'artist')));
		await check('[CF-L11c]', ['cf-scope-artist'], () =>
			ids(filter(items, `${E_DECOMPOSED}cho`, 'artist'))
		);
		await check('[CF-L11d]', [], () => ids(filter(items, `${E_DECOMPOSED}cho`, 'title')));
		await check('[CF-L11e]', ['cf-scope-title'], () =>
			ids(filter(items, `${E_DECOMPOSED}clair`, 'both'))
		);
		// The decomposed id fallback is searchable under title/both by its
		// exact byte spelling only, never under artist.
		const untitled: LibraryItem[] = [{ id: `canon-${E_DECOMPOSED}-id` }];
		await check('[CF-L11f]', [`canon-${E_DECOMPOSED}-id`], () =>
			ids(filter(untitled, `canon-${E_DECOMPOSED}-id`, 'title'))
		);
		await check('[CF-L11g]', [], () =>
			ids(filter(untitled, `canon-${E_DECOMPOSED}-id`, 'artist'))
		);
	});

	it('[CF-L12] metacharacters stay literal around canonical content', async () => {
		const filter = await requireFilter();
		const items: LibraryItem[] = [
			{ id: 'cf-literal', title: `Caf${E_ACUTE} (Live) [x]+`, artist: 'C++ Players' },
			{ id: 'cf-literal-plain', title: 'Just Words Here' }
		];
		await check('[CF-L12a]', ['cf-literal'], () => ids(filter(items, '(live)')));
		await check('[CF-L12b]', ['cf-literal'], () => ids(filter(items, '[x]+')));
		await check('[CF-L12c]', ['cf-literal'], () => ids(filter(items, 'c++')));
		await check('[CF-L12d]', ['cf-literal'], () => ids(filter(items, `caf${E_DECOMPOSED}`)));
		// A user-generated regular expression would match here; a literal
		// matcher must not.
		await check('[CF-L12e]', [], () => ids(filter(items, 'caf.')));
		await check('[CF-L12f]', [], () => ids(filter(items, '^Just')));
	});

	it('[CF-L13] empty, whitespace-only and non-string queries return every row', async () => {
		const filter = await requireFilter();
		const items: LibraryItem[] = [
			{ id: 'cf-empty-a', title: `${E_ACUTE}clair` },
			{ id: 'cf-empty-b', title: `${HANGUL_GA} Morning` }
		];
		const all = ['cf-empty-a', 'cf-empty-b'];
		for (const query of ['', '   ', String.fromCharCode(0xa0), ' \t\n ']) {
			const result = filter(items, query);
			const label = `[CF-L13:${JSON.stringify(query)}]`;
			await check(label, all, () => ids(result));
			expect(result, label).not.toBe(items);
			expect(result[0], label).toBe(items[0]);
		}
		for (const query of [null, undefined, 42, true, {}, []]) {
			const label = `[CF-L13:${JSON.stringify(query) ?? String(query)}]`;
			let result: LibraryItem[] | undefined;
			expect(() => {
				result = filter(items, query);
			}, `${label} must not throw`).not.toThrow();
			await check(label, all, () => ids(result as LibraryItem[]));
		}
	});

	it('[CF-L14] canonical results keep input order and references, never mutating frozen input', async () => {
		const filter = await requireFilter();
		const items = Object.freeze([
			Object.freeze({ id: 'cf-order-z', title: `${E_ACUTE}clair Song`, artist: 'Zed' }),
			Object.freeze({ id: 'cf-order-a', title: `${E_DECOMPOSED}clair Song`, artist: 'Alpha' }),
			Object.freeze({ id: 'cf-order-m', title: `SAME ${E_ACUTE_UPPER}CLAIR SONG`, artist: 'Mike' }),
			Object.freeze({ id: 'cf-order-other', title: 'Different', artist: 'Zulu' })
		]) as readonly LibraryItem[];
		const before = JSON.stringify(items);
		const result = filter(items, `${E_DECOMPOSED}clair song`);
		await check('[CF-L14]', ['cf-order-z', 'cf-order-a', 'cf-order-m'], () => ids(result));
		expect(result).not.toBe(items);
		expect(result[0]).toBe(items[0]);
		expect(result[1]).toBe(items[1]);
		expect(result[2]).toBe(items[2]);
		expect(JSON.stringify(items)).toBe(before);
		// Stored metadata keeps its original spelling: composed stays
		// composed, decomposed stays decomposed.
		expect(items[0]?.title).toBe(`${E_ACUTE}clair Song`);
		expect(items[1]?.title).toBe(`${E_DECOMPOSED}clair Song`);
	});
});
