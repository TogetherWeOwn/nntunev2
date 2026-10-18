import { describe, expect, it, vi } from 'vitest';
import {
	applyMediaSession,
	clearMediaSession,
	type MediaMetadataInit,
	type MediaSessionLike
} from './mediaSession.js';
import { DEMO_TRACKS } from './queue.js';

/**
 * Handler isolation: one rejected action must not take down the rest of
 * the transport wiring. The existing suites only throw for `pause` (the
 * last action), which cannot prove later actions survive an *early*
 * failure. These cases throw at `nexttrack` / `previoustrack` and pin
 * that every registration is still attempted.
 */

const BASE = 'https://example.test';
const ACTIONS = ['nexttrack', 'previoustrack', 'play', 'pause'] as const;
type Action = (typeof ACTIONS)[number];

const ACTION_TO_HANDLER = {
	nexttrack: 'onNext',
	previoustrack: 'onPrevious',
	play: 'onPlay',
	pause: 'onPause'
} as const;

class FakeMetadata {
	constructor(readonly init: MediaMetadataInit) {}
}

function handlerSet() {
	return {
		onNext: vi.fn(),
		onPrevious: vi.fn(),
		onPlay: vi.fn(),
		onPause: vi.fn()
	};
}

function isolatedSession(failingAction: Action | null, attempts: Action[]) {
	const handlers = new Map<string, (() => void) | null>();
	const session: MediaSessionLike = {
		metadata: null,
		setActionHandler(action, handler) {
			attempts.push(action as Action);
			if (action === failingAction) throw new Error(`unsupported: ${action}`);
			handlers.set(action, handler);
		}
	};
	return { session, handlers };
}

describe('mediaSession early handler isolation', () => {
	it.each(['nexttrack', 'previoustrack'] as const)(
		'attempts all four registrations and completes metadata when %s rejects',
		(failingAction) => {
			const attempts: Action[] = [];
			const { session, handlers } = isolatedSession(null, attempts);
			const options = { session, MetadataCtor: FakeMetadata, baseUrl: BASE };

			// Prepopulate old callbacks so the test proves replacement, not
			// just presence: only the new set may execute afterwards.
			const oldCallbacks = handlerSet();
			expect(applyMediaSession(DEMO_TRACKS[1]!, oldCallbacks, options)).toBe(true);
			expect(session.metadata).toBeInstanceOf(FakeMetadata);

			// Install the new set with the early failure armed.
			const throwing = isolatedSession(failingAction, attempts);
			// Reuse the populated handler map so "old" callbacks are live
			// when the failing install runs; attempts restarts for the new set.
			for (const [action, handler] of handlers) throwing.handlers.set(action, handler);
			throwing.session.metadata = session.metadata;
			attempts.length = 0;
			const freshCallbacks = handlerSet();

			let applied: boolean | undefined;
			expect(() => {
				applied = applyMediaSession(DEMO_TRACKS[1]!, freshCallbacks, {
					session: throwing.session,
					MetadataCtor: FakeMetadata,
					baseUrl: BASE
				});
			}).not.toThrow();

			// All four registrations are attempted in order despite the early throw.
			expect(applied).toBe(true);
			expect(attempts).toEqual([...ACTIONS]);
			// Metadata still completes with the valid track.
			expect(throwing.session.metadata).toBeInstanceOf(FakeMetadata);
			expect((throwing.session.metadata as FakeMetadata).init.title).toBe(
				DEMO_TRACKS[1]?.title
			);

			// The rejected action keeps its prior callback: no claim is made
			// that the rejected action itself was replaced.
			expect(throwing.handlers.get(failingAction)).toBe(
				oldCallbacks[ACTION_TO_HANDLER[failingAction]]
			);

			// Every surviving action runs only the new callback.
			for (const action of ACTIONS) {
				if (action === failingAction) continue;
				const key = ACTION_TO_HANDLER[action];
				expect(throwing.handlers.get(action), action).toBe(freshCallbacks[key]);
				throwing.handlers.get(action)?.();
				expect(freshCallbacks[key]).toHaveBeenCalledOnce();
				expect(oldCallbacks[key]).not.toHaveBeenCalled();
			}
		}
	);

	it('teardown attempts every clear after a first-clear failure and drops metadata', () => {
		const attempts: Action[] = [];
		const { session, handlers } = isolatedSession(null, attempts);
		const options = { session, MetadataCtor: FakeMetadata, baseUrl: BASE };

		applyMediaSession(DEMO_TRACKS[1]!, handlerSet(), options);
		expect(session.metadata).toBeInstanceOf(FakeMetadata);

		// Arm the failure on the first teardown clear (`nexttrack`) only.
		const tearingDown = isolatedSession('nexttrack', attempts);
		for (const [action, handler] of handlers) tearingDown.handlers.set(action, handler);
		tearingDown.session.metadata = session.metadata;
		attempts.length = 0;

		let cleared: boolean | undefined;
		expect(() => {
			cleared = clearMediaSession({
				session: tearingDown.session,
				MetadataCtor: FakeMetadata,
				baseUrl: BASE
			});
		}).not.toThrow();

		expect(cleared).toBe(true);
		// Subsequent handlers are still attempted after the first throw.
		expect(attempts).toEqual([...ACTIONS]);
		expect(tearingDown.session.metadata).toBeNull();
		// Later clears land; the rejected first action keeps its prior
		// callback, so it is deliberately not asserted as cleared.
		expect(tearingDown.handlers.get('previoustrack')).toBeNull();
		expect(tearingDown.handlers.get('play')).toBeNull();
		expect(tearingDown.handlers.get('pause')).toBeNull();
		expect(tearingDown.handlers.get('nexttrack')).toBeTypeOf('function');
	});
});
