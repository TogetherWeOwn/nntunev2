import { describe, expect, it } from 'vitest';
import { appendSelection } from '../library/selection.js';
import { createQueue } from './queue.js';

/**
 * Acceptance: find queued tracks across canonically equivalent Unicode
 * spellings (pure half of the SHARED queue matcher).
 *
 * OWNERSHIP: this file is acceptance-owned. The paired build lands the
 * canonical matcher on top of this branch and must make these tests pass
 * WITHOUT editing this file; a wrong test comes back to the acceptance
 * lane instead of being fixed beside the implementation. The
 * mocked-browser half is `apps/web/tests/canonical-track-find.spec.ts`
 * (library queue and demo queue surfaces); the loaded-library pure half
 * is `apps/web/src/lib/library/canonical-find-acceptance.test.ts`. The
 * pre-existing banks `demoQueueFilter.acceptance.test.ts` and
 * `playerQueue.filter.acceptance.test.ts` stay protected and unedited:
 * every behavior they pin must keep passing.
 *
 * Contract the build must provide — a compatible extension of the shipped
 * shared matcher `matchesQueueQuery(track, query)` in
 * `apps/web/src/lib/player/queueFilter.ts`, used by BOTH the library
 * PlayerQueue panel and the demo queue panel:
 * - Compare with ONE canonical helper applying the existing trim/lowercase
 *   rules plus NFC canonical normalization to the comparison strings on
 *   BOTH sides (stored metadata and query). Canonically equivalent title
 *   or artist spellings match in BOTH query directions.
 * - The shared combined-field shape is preserved: one trimmed lowercase
 *   substring over the title-or-id display text plus the artist, so a
 *   query spanning the title/artist boundary still matches.
 * - Accents stay significant: `e` never matches the accented vowels. No
 *   NFKC/NFKD, no mark stripping, no fuzzy matching, no transliteration,
 *   no user regex. Compatibility-only equivalents never match their
 *   ASCII lookalikes.
 * - Canonical-equivalence expansion applies to title/artist METADATA
 *   only. The stable-id title fallback stays byte-literal except for its
 *   existing case-fold/trim policy: a blank title rendering as an id that
 *   contains a decomposed sequence never matches its composed spelling
 *   and vice versa. Named tracks are still never matched by id, a
 *   missing artist still matches as empty (never the string
 *   "undefined"), and the track is never mutated.
 * - An empty or whitespace-only query still matches every track.
 *
 * RED shape: the loader below resolves the shipped matcher and asserts it
 * exists, so RED always means the canonical behavior is missing (the
 * baseline compares byte-literally), never that the harness cannot load.
 * CF-QC controls use ASCII/canonical-stable vectors and must pass before
 * AND after the build.
 *
 * Case IDs (stable): CF-QC00 fixture byte-shape guard, CF-QC01/CF-QC02
 * baseline controls, CF-Q01 title both directions, CF-Q02 artist both
 * directions, CF-Q03 combined marks and Hangul, CF-Q04 accent-negative,
 * CF-Q05 compatibility-negative, CF-Q06 id fallback byte-literal, CF-Q07
 * named tracks never match by id, CF-Q08 empty queries, CF-Q09 literals
 * and boundary semantics, CF-Q10 immutability, CF-Q11 fallback artists,
 * CF-Q12 literal-id/canonical-artist boundaries, CF-Q13 constructed fallback
 * id controls, CF-Q14 real metadata titles equal to ids, CF-Q15 named boundaries.
 *
 * Focused command (repo root, deterministic, offline):
 *   pnpm --dir apps/web exec vitest run src/lib/player/canonical-queue-find-acceptance.test.ts
 */

// Canonical pairs. Each constant's code points are pinned numerically by
// [CF-QC00], so an editor normalizing this file fails loudly there
// instead of silently weakening the matching cases. Outside these
// constants the file is pure ASCII. Scalar identities mirror the
// loaded-library half:
//   E_ACUTE U+00E9, E_DECOMPOSED e+U+0301, E_ACUTE_UPPER U+00C9,
//   A_RING U+00C5, A_RING_DECOMPOSED A+U+030A,
//   A_CIRCUMFLEX_ACUTE U+1EA5, A_CIRCUMFLEX_ACUTE_DECOMPOSED a+U+0302+U+0301,
//   HANGUL_GA U+AC00, HANGUL_GA_DECOMPOSED U+1100+U+1161,
//   LIGATURE_FI U+FB01, CIRCLED_ONE U+2460, FULLWIDTH_A U+FF21,
//   ROMAN_EIGHT U+2167.
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

type MatchTrack = { id: string; title: string; artist?: string | null };
type Matcher = (track: MatchTrack, query: string) => boolean;

async function loadMatcher(): Promise<Matcher | undefined> {
	try {
		const created = await import('./queueFilter.js');
		const fn = (created as Record<string, unknown>)['matchesQueueQuery'];
		if (typeof fn === 'function') return fn as Matcher;
	} catch {
		// Missing module: fall through to the missing-behavior assertion.
	}
	return undefined;
}

const MISSING =
	'missing behavior: extend matchesQueueQuery(track, query) in ' +
	'apps/web/src/lib/player/queueFilter.ts with NFC canonical equivalence ' +
	'on title/artist comparison (ids stay byte-literal) — contract header of ' +
	'apps/web/src/lib/player/canonical-queue-find-acceptance.test.ts';

async function requireMatcher(): Promise<Matcher> {
	const fn = await loadMatcher();
	expect(fn, MISSING).toBeDefined();
	return fn as Matcher;
}

const points = (value: string): (number | undefined)[] =>
	[...value].map((character) => character.codePointAt(0));

function receipt(case_id: string, expected: unknown, actual: unknown, status: 'PASS' | 'FAIL') {
	console.log(
		JSON.stringify({
			case_id,
			expected,
			actual,
			status,
			error_code: status === 'PASS' ? null : 'CANONICAL_QUEUE_FIND_CONTRACT',
			next_action:
				status === 'PASS'
					? 'none'
					: 'Apply NFC canonical equivalence to the shared queue title/artist matching; keep ids byte-literal.'
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

describe('canonical queue find: fixture guard and baseline controls (pass before and after)', () => {
	it('[CF-QC00] fixture constants hold their intended code points and NFC relations', () => {
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
		expect(E_DECOMPOSED.normalize('NFC')).toBe(E_ACUTE);
		expect(A_RING_DECOMPOSED.normalize('NFC')).toBe(A_RING);
		expect(A_CIRCUMFLEX_ACUTE_DECOMPOSED.normalize('NFC')).toBe(A_CIRCUMFLEX_ACUTE);
		expect(HANGUL_GA_DECOMPOSED.normalize('NFC')).toBe(HANGUL_GA);
		for (const stable of [E_ACUTE, LIGATURE_FI, CIRCLED_ONE, FULLWIDTH_A, ROMAN_EIGHT]) {
			expect(stable.normalize('NFC')).toBe(stable);
		}
		receipt('[CF-QC00]', 'fixture byte shapes intact', 'fixture byte shapes intact', 'PASS');
	});

	it('[CF-QC01] the shipped ASCII match and id fallback are unchanged', async () => {
		const matches = await requireMatcher();
		const named = { id: 'cf-queue-named', title: 'Coral Night', artist: 'Mira North' };
		await check('[CF-QC01a]', true, () => matches(named, '  cOrAl  '));
		await check('[CF-QC01b]', true, () => matches(named, 'mira north'));
		await check('[CF-QC01c]', false, () => matches(named, 'cf-queue-named'));
		await check('[CF-QC01d]', true, () =>
			matches({ id: 'blank-stable-id', title: '   ' }, ' STABLE ')
		);
	});

	it('[CF-QC02] composed-only accented metadata keeps its shipped behavior', async () => {
		const matches = await requireMatcher();
		const track = { id: 'cf-queue-accent', title: `${E_ACUTE}toile Song`, artist: 'North' };
		await check('[CF-QC02]', true, () => matches(track, ` ${E_ACUTE_UPPER}TOILE `));
	});
});

describe('canonical queue find: pure matching over queued tracks', () => {
	it('[CF-Q01] a composed title matches a decomposed query and vice versa', async () => {
		const matches = await requireMatcher();
		const composed = { id: 'cf-q-title-composed', title: `${E_ACUTE}clair`, artist: 'North' };
		const decomposed = { id: 'cf-q-title-decomposed', title: `${E_DECOMPOSED}clair`, artist: 'North' };
		await check('[CF-Q01a]', true, () => matches(composed, `${E_DECOMPOSED}clair`));
		await check('[CF-Q01b]', true, () => matches(decomposed, `${E_ACUTE}clair`));
		await check('[CF-Q01c]', true, () => matches(decomposed, ` ${E_ACUTE_UPPER}CLAIR `));
		await check('[CF-Q01d]', false, () => matches(composed, 'unrelated tune'));
	});

	it('[CF-Q02] artist matching is canonically equivalent in both directions', async () => {
		const matches = await requireMatcher();
		const composed = { id: 'cf-q-artist-composed', title: 'Northern Song', artist: `${A_RING}ngstrom` };
		const decomposed = {
			id: 'cf-q-artist-decomposed',
			title: 'Southern Song',
			artist: `${A_RING_DECOMPOSED}ngstrom`
		};
		await check('[CF-Q02a]', true, () => matches(composed, `${A_RING_DECOMPOSED}ngstrom`));
		await check('[CF-Q02b]', true, () => matches(decomposed, `${A_RING}ngstrom`));
		await check('[CF-Q02c]', false, () => matches(composed, 'nobody'));
	});

	it('[CF-Q03] combined marks and Hangul match their canonical equivalents', async () => {
		const matches = await requireMatcher();
		const combinedComposed = {
			id: 'cf-q-combined-composed',
			title: `S${A_CIRCUMFLEX_ACUTE}o Paulo Nights`,
			artist: 'North'
		};
		const combinedDecomposed = {
			id: 'cf-q-combined-decomposed',
			title: `S${A_CIRCUMFLEX_ACUTE_DECOMPOSED}o Paulo Days`,
			artist: 'North'
		};
		const hangulComposed = { id: 'cf-q-hangul-composed', title: `${HANGUL_GA} Morning`, artist: 'North' };
		const hangulDecomposed = {
			id: 'cf-q-hangul-decomposed',
			title: `${HANGUL_GA_DECOMPOSED} Evening`,
			artist: 'North'
		};
		await check('[CF-Q03a]', true, () =>
			matches(combinedComposed, `s${A_CIRCUMFLEX_ACUTE_DECOMPOSED}o`)
		);
		await check('[CF-Q03b]', true, () =>
			matches(combinedDecomposed, `S${A_CIRCUMFLEX_ACUTE}o`)
		);
		await check('[CF-Q03c]', true, () => matches(hangulComposed, HANGUL_GA_DECOMPOSED));
		await check('[CF-Q03d]', true, () => matches(hangulDecomposed, ` ${HANGUL_GA} `));
	});

	it('[CF-Q04] accents stay significant: bare e never matches accented vowels', async () => {
		const matches = await requireMatcher();
		const accented = { id: 'cf-q-accented', title: `${E_ACUTE}clair`, artist: `${E_ACUTE}cho` };
		const bare = { id: 'cf-q-bare', title: 'Eclair', artist: 'Echo' };
		await check('[CF-Q04a]', false, () => matches(accented, 'eclair'));
		await check('[CF-Q04b]', true, () => matches(bare, 'ECLAIR'));
		await check('[CF-Q04c]', true, () => matches(accented, `${E_ACUTE}clair`));
		await check('[CF-Q04d]', false, () => matches(accented, 'echo'));
	});

	it('[CF-Q05] compatibility-only lookalikes never match', async () => {
		const matches = await requireMatcher();
		const fi = { id: 'cf-q-fi', title: `${LIGATURE_FI}nishing Song`, artist: 'North' };
		const circled = { id: 'cf-q-circled', title: `Take ${CIRCLED_ONE}`, artist: 'North' };
		const fullwidth = { id: 'cf-q-fullwidth', title: `${FULLWIDTH_A}pple Song`, artist: 'North' };
		const roman = { id: 'cf-q-roman', title: `Part ${ROMAN_EIGHT}`, artist: 'North' };
		const ascii = { id: 'cf-q-ascii', title: 'Finishing Take Apple Part', artist: 'North' };
		await check('[CF-Q05a]', false, () => matches(fi, 'finishing'));
		await check('[CF-Q05b]', true, () => matches(ascii, 'finishing'));
		await check('[CF-Q05c]', true, () => matches(fi, `${LIGATURE_FI}nishing`));
		await check('[CF-Q05d]', false, () => matches(circled, 'take 1'));
		await check('[CF-Q05e]', true, () => matches(circled, `take ${CIRCLED_ONE}`));
		await check('[CF-Q05f]', false, () => matches(fullwidth, 'apple'));
		await check('[CF-Q05g]', true, () => matches(fullwidth, `${FULLWIDTH_A}pple`));
		await check('[CF-Q05h]', false, () => matches(roman, 'part viii'));
		await check('[CF-Q05i]', true, () => matches(roman, `part ${ROMAN_EIGHT}`));
	});

	it('[CF-Q06] a blank title rendering as an id stays byte-literal in both directions', async () => {
		const matches = await requireMatcher();
		const decomposedId = { id: `canon-${E_DECOMPOSED}-id`, title: '   ' };
		const composedId = { id: `canon-${E_ACUTE}-id`, title: '' };
		await check('[CF-Q06a]', false, () => matches(decomposedId, `canon-${E_ACUTE}-id`));
		await check('[CF-Q06b]', false, () => matches(composedId, `canon-${E_DECOMPOSED}-id`));
		await check('[CF-Q06c]', true, () => matches(decomposedId, ` CANON-${E_DECOMPOSED}-ID `));
		await check('[CF-Q06d]', true, () => matches(composedId, `canon-${E_ACUTE}-id`));
		await check('[CF-Q06e]', true, () =>
			matches({ id: 'empty-title-id', title: '' }, 'TITLE-ID')
		);
	});

	it('[CF-Q07] named tracks are still never matched by id', async () => {
		const matches = await requireMatcher();
		const named = { id: 'cf-q-named-id', title: `${E_ACUTE}clair Song`, artist: 'North' };
		await check('[CF-Q07a]', false, () => matches(named, 'cf-q-named-id'));
		await check('[CF-Q07b]', false, () => matches(named, 'CF-Q-NAMED-ID'));
		// The named track still matches by its metadata (canonical title
		// directions are pinned by CF-Q01 instead).
		await check('[CF-Q07c]', true, () => matches(named, `${E_ACUTE}clair`));
	});

	it('[CF-Q08] an empty or whitespace-only query matches every track', async () => {
		const matches = await requireMatcher();
		const track = { id: 'cf-q-empty', title: `${E_ACUTE}clair`, artist: `${HANGUL_GA} Band` };
		await check('[CF-Q08a]', true, () => matches(track, ''));
		await check('[CF-Q08b]', true, () => matches(track, '   '));
		await check('[CF-Q08c]', true, () => matches(track, ' \t\n '));
		await check('[CF-Q08d]', true, () => matches(track, String.fromCharCode(0xa0)));
	});

	it('[CF-Q09] literals stay literal and the title/artist boundary still matches', async () => {
		const matches = await requireMatcher();
		const literal = { id: 'cf-q-literal', title: '[Live] .* Song', artist: 'North' };
		const canonical = { id: 'cf-q-boundary', title: `Caf${E_ACUTE}`, artist: 'Live Band' };
		await check('[CF-Q09a]', true, () => matches(literal, '.*'));
		await check('[CF-Q09b]', false, () => matches(canonical, '.*'));
		await check('[CF-Q09c]', false, () => matches(literal, 'a.c'));
		// Boundary: the query spans the joined "title artist" text.
		await check('[CF-Q09d]', true, () => matches(canonical, `caf${E_DECOMPOSED} live`));
		await check('[CF-Q09e]', false, () => matches({ id: 'cf-q-noartist', title: 'Solo' }, 'undefined'));
	});

	it('[CF-Q10] matching never mutates the track', async () => {
		const matches = await requireMatcher();
		const track = Object.freeze({
			id: 'cf-q-frozen',
			title: `${E_DECOMPOSED}clair Song`,
			artist: `${A_RING_DECOMPOSED}ngstrom`
		});
		const before = JSON.stringify(track);
		const found = matches(track, `${E_ACUTE}clair`);
		await check('[CF-Q10]', true, () => found);
		expect(JSON.stringify(track)).toBe(before);
		// Stored metadata keeps its original spelling.
		expect(track.title).toBe(`${E_DECOMPOSED}clair Song`);
		expect(track.artist).toBe(`${A_RING_DECOMPOSED}ngstrom`);
	});
});

function enqueueLibraryItem(id: string, title: string, artist?: string): MatchTrack {
	const result = appendSelection(createQueue([]), [id], [{ id, title, artist }]);
	expect(result.added).toBe(1);
	expect(result.skipped).toBe(0);
	expect(result.queue.tracks).toHaveLength(1);
	return result.queue.tracks[0]!;
}

// Missing-title provenance comes from the real library-to-queue conversion,
// never from a hand-built { title: id } object or an invented marker.
const fallbackForms = [
	{ label: 'blank title', queued: false, title: '' },
	{ label: 'whitespace title', queued: false, title: '   ' },
	{ label: 'queued blank library title', queued: true, title: '' },
	{ label: 'queued whitespace library title', queued: true, title: '   ' }
];

describe.each(fallbackForms)('canonical queue fallback: $label', ({ label, queued, title }) => {
	function fallback(id: string, artist?: string): MatchTrack {
		if (!queued) return { id, title, artist };
		const track = enqueueLibraryItem(id, title, artist);
		expect(track.id).toBe(id);
		expect(track.title).toBe(id);
		return track;
	}

	it.each([
		{
			direction: 'composed artist, decomposed query',
			artist: `Caf${E_ACUTE} Band`, query: `caf${E_DECOMPOSED} band`
		},
		{
			direction: 'decomposed artist, composed query',
			artist: `Caf${E_DECOMPOSED} Band`, query: `CAF${E_ACUTE_UPPER} BAND`
		}
	])('[CF-Q11] artist-only equivalence: $direction', async ({ direction, artist, query }) => {
		const matches = await requireMatcher();
		await check(`[CF-Q11] ${label}: ${direction}`, true, () =>
			matches(fallback('plain-fallback-id', artist), query)
		);
	});

	it.each([
		{ query: `caf${E_ACUTE}`, artist: undefined },
		{ query: `caf${E_DECOMPOSED}`, artist: '' },
		{ query: 'undefined', artist: undefined },
		{ query: 'null', artist: undefined }
	])('[CF-Q11] missing artist never matches $query', async ({ query, artist }) => {
		const matches = await requireMatcher();
		await check(`[CF-Q11] ${label}: missing artist / ${query}`, false, () =>
			matches(fallback('plain-fallback-id', artist), query)
		);
	});

	it.each([
		{
			direction: 'ASCII id, composed artist',
			id: 'track-1', artist: `Caf${E_ACUTE}`,
			query: `track-1 caf${E_DECOMPOSED}`, expected: true
		},
		{
			direction: 'ASCII id, decomposed artist',
			id: 'track-1', artist: `Caf${E_DECOMPOSED}`,
			query: `track-1 caf${E_ACUTE}`, expected: true
		},
		{
			direction: 'ASCII id, literal decomposed artist and query',
			id: 'track-1', artist: `Caf${E_DECOMPOSED}`,
			query: `track-1 caf${E_DECOMPOSED}`, expected: true
		},
		{
			direction: 'spaced decomposed id, composed artist',
			id: `disc ${E_DECOMPOSED} side`, artist: `Caf${E_ACUTE} Live Band`,
			query: `${E_DECOMPOSED} side caf${E_DECOMPOSED} live`, expected: true
		},
		{
			direction: 'spaced composed id, decomposed artist',
			id: `disc ${E_ACUTE} side`, artist: `Caf${E_DECOMPOSED} Live Band`,
			query: `${E_ACUTE} side caf${E_ACUTE} live`, expected: true
		},
		{
			direction: 'decomposed id must not match composed query id',
			id: `disc ${E_DECOMPOSED} side`, artist: `Caf${E_DECOMPOSED} Live Band`,
			query: `${E_ACUTE} side caf${E_ACUTE} live`, expected: false
		},
		{
			direction: 'composed id must not match decomposed query id',
			id: `disc ${E_ACUTE} side`, artist: `Caf${E_ACUTE} Live Band`,
			query: `${E_DECOMPOSED} side caf${E_DECOMPOSED} live`, expected: false
		}
	])('[CF-Q12] literal id plus canonical artist: $direction', async ({ direction, id, artist, query, expected }) => {
		const matches = await requireMatcher();
		await check(`[CF-Q12] ${label}: ${direction}`, expected, () =>
			matches(fallback(id, artist), query)
		);
	});

	it.each([
		{
			direction: 'decomposed id, literal query', id: `canon-${E_DECOMPOSED}-id`,
			query: ` CANON-${E_DECOMPOSED}-ID `, expected: true
		},
		{
			direction: 'composed id, literal query', id: `canon-${E_ACUTE}-id`,
			query: `CANON-${E_ACUTE_UPPER}-ID`, expected: true
		},
		{
			direction: 'decomposed id, composed query', id: `canon-${E_DECOMPOSED}-id`,
			query: `canon-${E_ACUTE}-id`, expected: false
		},
		{
			direction: 'composed id, decomposed query', id: `canon-${E_ACUTE}-id`,
			query: `canon-${E_DECOMPOSED}-id`, expected: false
		}
	])('[CF-Q13] fallback ids stay literal: $direction', async ({ direction, id, query, expected }) => {
		const matches = await requireMatcher();
		await check(`[CF-Q13] ${label}: ${direction}`, expected, () =>
			matches(fallback(id), query)
		);
	});
});

describe('canonical queue find: real metadata must not be mistaken for an id fallback', () => {
	it.each([
		{
			direction: 'decomposed metadata, composed query',
			title: `caf${E_DECOMPOSED}`, query: `caf${E_ACUTE}`
		},
		{
			direction: 'composed metadata, decomposed query',
			title: `caf${E_ACUTE}`, query: `caf${E_DECOMPOSED}`
		}
	])('[CF-Q14] a nonblank title equal to its id stays metadata: $direction', async ({ direction, title, query }) => {
		const matches = await requireMatcher();
		const named = enqueueLibraryItem(title, title);
		expect(named.id).toBe(title);
		expect(named.title).toBe(title);
		await check(`[CF-Q14] ${direction}`, true, () => matches(named, query));
	});

	it.each([
		{
			direction: 'composed title, decomposed artist',
			title: `Caf${E_ACUTE} Night`, artist: `${A_RING_DECOMPOSED}ngstrom Band`,
			query: `caf${E_DECOMPOSED} night ${A_RING}ngstrom`
		},
		{
			direction: 'decomposed title, composed artist',
			title: `Caf${E_DECOMPOSED} Night`, artist: `${A_RING}ngstrom Band`,
			query: `caf${E_ACUTE} night ${A_RING_DECOMPOSED}ngstrom`
		}
	])('[CF-Q15] ordinary metadata boundaries stay canonical: $direction', async ({ direction, title, artist, query }) => {
		const matches = await requireMatcher();
		await check(`[CF-Q15] ${direction}`, true, () =>
			matches({ id: 'ordinary-metadata-id', title, artist }, query)
		);
	});
});
