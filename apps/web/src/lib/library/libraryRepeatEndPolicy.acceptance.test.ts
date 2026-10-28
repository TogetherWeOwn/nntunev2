import { describe, expect, it } from 'vitest';
import {
	createQueue,
	moveTrack,
	naturalEndPolicy,
	nextTrack,
	previousTrack,
	selectTrack,
	type QueueState,
	type RepeatMode,
	type Track
} from '../player/queue.js';

/**
 * Test-Author owned acceptance for LIBREPEAT: advance at end of track with an
 * explicit repeat Off/One/Queue control on `/library`. The Builder wires
 * `apps/web/src/routes/library/+page.svelte` to satisfy this file plus
 * `apps/web/tests/library-repeat-end-policy.spec.ts` and must not edit either
 * file.
 *
 * LOCKED CONTRACT (`apps/web/src/routes/library/+page.svelte`):
 * - A NATIVE <select> `player-repeat` (accessible name /repeat/i, values
 *   off/one/queue, labels Off/One/Queue, default Off) plus a `repeat-status`
 *   live region (role=status, text "Repeat: Off/One/Queue"). The mode persists
 *   across reload (local-state-bounds pattern); corrupt values fall back to Off.
 * - The native `<audio data-testid="audio-player">` carries an `ended` handler
 *   wired to the existing `naturalEndPolicy` core from `#lib/player/queue.js`:
 *   Off advances then stops truthfully at queue end; One replays current;
 *   Queue loops to first. Never autoplays from empty, never fabricates a track,
 *   never uses a native `loop` attribute. Auth teardown clears pending end
 *   handling; a fired sleep timer stops with no advance after it.
 * - End-of-track advance never disturbs queue order, favorites, subset
 *   selection, filters or media state beyond the intended selection change;
 *   share/resume seeks stay explicit. One concise polite announcement per
 *   automatic advance, none for silent transitions.
 * See the Playwright spec header for the full page contract.
 *
 * PROTECTED (Test-Author owned; Builder must not edit):
 * - `apps/web/src/lib/library/libraryRepeatEndPolicy.acceptance.test.ts` (this file)
 * - `apps/web/tests/library-repeat-end-policy.spec.ts`
 * BUILDER-OWNED implementation surface:
 * - `apps/web/src/routes/library/+page.svelte` (repeat control + status,
 *   ended wiring via naturalEndPolicy, persistence, announcements)
 * - Existing queue/resume/share/sleep suites stay unchanged.
 *
 * Run from apps/web (offline):
 *   pnpm test src/lib/library/libraryRepeatEndPolicy.acceptance.test.ts
 * Companion browser run (builds + serves locally; no staging/prod):
 *   pnpm exec playwright test tests/library-repeat-end-policy.spec.ts
 *
 * RED NOW: the library route has no repeat hooks and does not reference
 * `naturalEndPolicy`. GREEN CONTROLS: the no-loop pin and the
 * naturalEndPolicy vector pins pass on the current tree, proving this file
 * compiles and the reuse contract holds.
 * Every RED assertion message is a compact JSON receipt:
 * {"case","expected","actual","status","error_code","next_action"}.
 */

// Raw route markup via Vite glob: no node:fs, no @types/node needed.
const sources = import.meta.glob('../../routes/**/*.svelte', {
	query: '?raw',
	import: 'default',
	eager: true
}) as Record<string, string>;

const ROUTE = '../../routes/library/+page.svelte';

function routeSource(): string {
	const source = sources[ROUTE];
	expect(
		source,
		JSON.stringify({
			case: 'LIBREP-ROUTE',
			expected: `route source "${ROUTE}" to exist`,
			actual: 'missing',
			status: 'RED',
			error_code: 'route_missing',
			next_action: 'Do not move the library route; Builder adds repeat hooks there.'
		})
	).toBeDefined();
	return source ?? '';
}

/** Compact JSON receipt carried by every RED assertion message. */
function note(
	caseId: string,
	expected: string,
	actual: string,
	errorCode: string,
	nextAction: string
): string {
	return JSON.stringify({
		case: caseId,
		expected,
		actual,
		status: 'RED',
		error_code: errorCode,
		next_action: nextAction
	});
}

/** Library-shaped ids (never the demo catalogue): reuse must be id-agnostic. */
const LIBRARY_TRACKS: Track[] = [
	{ id: 'lib-coral', title: 'Coral Night', artist: 'Mira', streamUrl: '/test-audio/lib-coral' },
	{ id: 'lib-pine', title: 'Pine Morning', artist: 'North', streamUrl: '/test-audio/lib-pine' },
	{ id: 'lib-ember', title: 'Ember Line', artist: '', streamUrl: '/test-audio/lib-ember' }
];

type EndContext = {
	repeat: RepeatMode;
	radioActive: boolean;
	continueRadio: boolean;
	playing: boolean;
	validEnd: boolean;
	sleepComplete: boolean;
};

const BASE: EndContext = {
	repeat: 'off',
	radioActive: false,
	continueRadio: false,
	playing: true,
	validEnd: true,
	sleepComplete: false
};

function frozenQueue(index = 0): QueueState {
	const queue = selectTrack(createQueue(LIBRARY_TRACKS), index);
	queue.tracks.forEach(Object.freeze);
	Object.freeze(queue.tracks);
	return Object.freeze(queue);
}

describe('library repeat hooks are declared in the library markup', () => {
	it('LIBREP-U01 declares a native repeat select (player-repeat)', () => {
		expect(
			routeSource(),
			note(
				'LIBREP-U01',
				'data-testid="player-repeat" in the library markup',
				'missing',
				'repeat_control_missing',
				'Builder adds a native <select data-testid="player-repeat"> with values off/one/queue (see library-repeat-end-policy.spec.ts LOCKED CONTRACT).'
			)
		).toContain('data-testid="player-repeat"');
	});

	it('LIBREP-U02 declares the polite repeat status region (repeat-status)', () => {
		expect(
			routeSource(),
			note(
				'LIBREP-U02',
				'data-testid="repeat-status" in the library markup',
				'missing',
				'repeat_status_missing',
				'Builder adds the role=status repeat announcement region.'
			)
		).toContain('data-testid="repeat-status"');
	});

	it('LIBREP-U03 wires the native ended event on the library audio', () => {
		expect(
			routeSource(),
			note(
				'LIBREP-U03',
				'the library markup handles the audio ended event via onended',
				'no ended handler',
				'ended_handler_missing',
				'Builder adds onended to the library <audio> and routes it through naturalEndPolicy (no parallel end-policy).'
			)
		).toContain('onended');
	});

	it('LIBREP-U04 reuses naturalEndPolicy as the single end-of-track source', () => {
		expect(
			routeSource(),
			note(
				'LIBREP-U04',
				'the library route reuses naturalEndPolicy',
				'no reference',
				'end_policy_reuse_missing',
				'Builder imports naturalEndPolicy from #lib/player/queue.js and drives every library ended event through it.'
			)
		).toContain('naturalEndPolicy');
	});

	it('LIBREP-U05 never uses a native loop attribute (control: passes now, must stay green)', () => {
		const source = routeSource();
		for (const forbidden of ['loop=', 'loop={true}', '.loop = true']) {
			expect(
				source,
				note(
					'LIBREP-U05',
					`no "${forbidden}" in the library route`,
					'native loop present',
					'native_loop_forbidden',
					'Remove the native loop; One/Queue replay comes from naturalEndPolicy-driven play-from-zero, never the loop attribute.'
				)
			).not.toContain(forbidden);
		}
	});
});

describe('reused natural-end policy with library-shaped ids (controls: pass now, must stay green)', () => {
	it('LIBREP-U06 Off advances mid-queue then stops truthfully at the tail', () => {
		// The library has no radio pass: continueRadio stays false, so the tail
		// completes with a truthful stop instead of delegating to radio.
		const mid = naturalEndPolicy(frozenQueue(0), BASE);
		expect(mid.action).toBe('play');
		expect(mid.queue.currentIndex).toBe(1);
		expect(mid.queue.tracks).toEqual(LIBRARY_TRACKS);
		const tail = naturalEndPolicy(frozenQueue(2), BASE);
		expect(tail.action).toBe('stop');
		expect(tail.queue.currentIndex).toBe(2);
		expect(tail.queue.tracks).toEqual(LIBRARY_TRACKS);
	});

	it('LIBREP-U07 One replays the current track without moving the selection', () => {
		for (const index of [0, 1, 2]) {
			const queue = frozenQueue(index);
			const result = naturalEndPolicy(queue, { ...BASE, repeat: 'one' });
			expect(result.action).toBe('play');
			expect(result.queue.currentIndex).toBe(index);
			expect(result.queue.tracks).toEqual(LIBRARY_TRACKS);
		}
	});

	it('LIBREP-U08 Queue follows the edited order then wraps its last item', () => {
		const edited = moveTrack(createQueue(LIBRARY_TRACKS), 2, 0);
		const before = JSON.stringify(edited);
		const first = naturalEndPolicy(edited, { ...BASE, repeat: 'queue' });
		expect(first.action).toBe('play');
		expect(first.queue.tracks.map((track) => track.id)).toEqual([
			'lib-ember',
			'lib-coral',
			'lib-pine'
		]);
		expect(first.queue.currentIndex).toBe(2);
		const wrapped = naturalEndPolicy(first.queue, { ...BASE, repeat: 'queue' });
		expect(wrapped.action).toBe('play');
		expect(wrapped.queue.currentIndex).toBe(0);
		expect(wrapped.queue.tracks.map((track) => track.id)).toEqual([
			'lib-ember',
			'lib-coral',
			'lib-pine'
		]);
		expect(JSON.stringify(edited)).toBe(before);
	});

	it('LIBREP-U09 empty queues never advance and singletons never fabricate', () => {
		const empty = createQueue([]);
		for (const repeat of ['off', 'one', 'queue'] as const) {
			expect(naturalEndPolicy(empty, { ...BASE, repeat })).toEqual({
				queue: empty,
				action: 'ignore'
			});
		}
		const singleton = createQueue([LIBRARY_TRACKS[1]!]);
		expect(naturalEndPolicy(singleton, BASE)).toEqual({ queue: singleton, action: 'stop' });
		for (const repeat of ['one', 'queue'] as const) {
			expect(naturalEndPolicy(singleton, { ...BASE, repeat })).toEqual({
				queue: singleton,
				action: 'play'
			});
		}
	});

	it('LIBREP-U10 sleep completion overrides every repeat mode', () => {
		for (const repeat of ['off', 'one', 'queue'] as const) {
			const queue = frozenQueue(1);
			expect(naturalEndPolicy(queue, { ...BASE, repeat, sleepComplete: true })).toEqual({
				queue,
				action: 'stop'
			});
		}
	});

	it('LIBREP-U11 paused, stale and empty ends stay inert under every mode', () => {
		for (const repeat of ['off', 'one', 'queue'] as const) {
			const queue = frozenQueue(1);
			for (const patch of [{ playing: false }, { validEnd: false }]) {
				expect(naturalEndPolicy(queue, { ...BASE, repeat, ...patch })).toEqual({
					queue,
					action: 'ignore'
				});
			}
		}
	});

	it('LIBREP-U12 explicit navigation remains clamped (known-good control)', () => {
		const first = frozenQueue(0);
		const last = frozenQueue(2);
		expect(nextTrack(first).currentIndex).toBe(1);
		expect(nextTrack(last)).toEqual(last);
		expect(previousTrack(first)).toEqual(first);
		expect(previousTrack(last).currentIndex).toBe(1);
	});
});
