import { describe, expect, it } from 'vitest';

/**
 * DEMOFIND — test-only acceptance handoff, test/demo-queue-filter-acceptance.
 * Protected test bank: this file and tests/demo-queue-filter.acceptance.spec.ts.
 * Builder-owned implementation (NOT protected tests): queueFilter.ts,
 * PlayerQueue.svelte, and ../../routes/demo/player/+page.svelte. Extract one
 * matchesQueueQuery(track, query) export from queueFilter.ts and have BOTH
 * consumers use it. Do not alter PlayerQueue behavior or its existing suites.
 *
 * Offline setup: pnpm --dir apps/web install --offline --frozen-lockfile --prod=false
 * Focused RED/GREEN: pnpm --dir apps/web exec vitest run src/lib/player/demoQueueFilter.acceptance.test.ts
 * Regression: pnpm --dir apps/web exec vitest run src/lib/player/playerQueue.filter.acceptance.test.ts
 * Browser instructions and state-isolation contract: companion spec header.
 * JSON lines: case_id, expected, actual, status, error_code, next_action.
 * A missing export is an ASSERTION failure, not an import/collection error.
 * Each vector names a realistic break: normalization, wrong field, ID leakage,
 * missing fallback, regex interpretation, Unicode handling or input mutation.
 * Builder owns the combined GREEN PR; no intentionally RED-only ready PR.
 */

type MatchTrack = { id: string; title: string; artist?: string };
type Matcher = (track: MatchTrack, query: string) => boolean;
const modules = import.meta.glob<{ matchesQueueQuery?: Matcher }>('./queueFilter.ts', { eager: true });
const matcher = modules['./queueFilter.ts']?.matchesQueueQuery;
const named = Object.freeze({ id: 'private-track-id', title: 'Coral Night', artist: 'Mira North' });

const vectors: { id: string; track: MatchTrack; query: string; expected: boolean }[] = [
	{ id: 'DEMOFIND-U01', track: named, query: '  cOrAl  ', expected: true },
	{ id: 'DEMOFIND-U02', track: named, query: '  rA No  ', expected: true },
	{ id: 'DEMOFIND-U03', track: named, query: ' \t\n ', expected: true },
	{ id: 'DEMOFIND-U04', track: { id: 'blank-stable-id', title: ' \t ', artist: '' }, query: ' STABLE ', expected: true },
	{ id: 'DEMOFIND-U05', track: { id: 'empty-title-id', title: '' }, query: 'TITLE-ID', expected: true },
	{ id: 'DEMOFIND-U06', track: named, query: 'private-track-id', expected: false },
	{ id: 'DEMOFIND-U07', track: { id: 'unicode', title: 'Étoile du soir', artist: 'Björk' }, query: ' ÉTOILE ', expected: true },
	{ id: 'DEMOFIND-U08', track: { id: 'unicode', title: 'Étoile du soir', artist: 'Björk' }, query: ' BJÖ ', expected: true },
	{ id: 'DEMOFIND-U09', track: { id: 'literal', title: '[Live] .* <b>Song</b>' }, query: '.*', expected: true },
	{ id: 'DEMOFIND-U10', track: named, query: '.*', expected: false },
	{ id: 'DEMOFIND-U11', track: { id: 'no-artist', title: 'Solo' }, query: 'undefined', expected: false },
	{ id: 'DEMOFIND-U12', track: named, query: 'not in this queue', expected: false }
];

describe('shared queue presentation matcher', () => {
	for (const vector of vectors) {
		it(`${vector.id} ${JSON.stringify(vector.query)} => ${vector.expected}`, () => {
			const track = Object.freeze({ ...vector.track });
			const before = JSON.stringify(track);
			const expected = { matches: vector.expected, unchanged: true };
			let actual: unknown = { error: 'shared matcher export missing' };
			let passed = false;
			try {
				if (typeof matcher === 'function') {
					actual = { matches: matcher(track, vector.query), unchanged: JSON.stringify(track) === before };
				}
				expect(actual).toEqual(expected);
				passed = true;
			} finally {
				console.log(JSON.stringify({ case_id: vector.id, expected, actual,
					status: passed ? 'PASS' : 'FAIL', error_code: passed ? null : 'QUEUE_MATCH_CONTRACT',
					next_action: passed ? 'none' : 'Implement the shared matcher; preserve the protected test bank.' }));
			}
		});
	}
});
