import { describe, expect, it, vi } from 'vitest';
import {
	applyMediaSession,
	clearMediaSession,
	type MediaMetadataInit,
	type MediaSessionLike
} from './mediaSession.js';
import { DEMO_TRACKS } from './queue.js';

/**
 * Teardown metadata-setter isolation: `clearMediaSession` (track === null
 * path) sets `session.metadata = null` before clearing any action handler.
 * A setter that rejects null must not prevent the four handler clears or
 * throw out of teardown; OS controls are global and would otherwise keep
 * driving a destroyed page.
 */

const BASE = 'https://example.test';
const ACTIONS = ['nexttrack', 'previoustrack', 'play', 'pause'] as const;
type Action = (typeof ACTIONS)[number];

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

/**
 * Session whose metadata setter throws only on null assignment (the
 * teardown clear). Non-null metadata stores normally so the prepopulate
 * step can establish a live session.
 */
function nullRejectingSession(attempts: Action[], failingAction: Action | null) {
	const handlers = new Map<string, (() => void) | null>();
	let stored: unknown = null;
	const session: MediaSessionLike = {
		get metadata(): unknown {
			return stored;
		},
		set metadata(value: unknown) {
			if (value === null) throw new Error('metadata null rejected');
			stored = value;
		},
		setActionHandler(action, handler) {
			attempts.push(action as Action);
			if (action === failingAction) throw new Error(`unsupported: ${action}`);
			handlers.set(action, handler);
		}
	};
	return { session, handlers, setStored: (v: unknown) => (stored = v) };
}

describe('mediaSession teardown metadata-setter isolation', () => {
	it('clears all four handlers without throwing when metadata=null rejects', () => {
		const attempts: Action[] = [];
		const { session, handlers } = nullRejectingSession(attempts, null);
		const options = { session, MetadataCtor: FakeMetadata, baseUrl: BASE };

		// Prepopulate callbacks + metadata so teardown proves replacement.
		applyMediaSession(DEMO_TRACKS[1]!, handlerSet(), options);
		expect(session.metadata).toBeInstanceOf(FakeMetadata);
		expect(handlers.get('nexttrack')).toBeTypeOf('function');
		attempts.length = 0;

		let cleared: boolean | undefined;
		expect(() => {
			cleared = clearMediaSession(options);
		}).not.toThrow();

		expect(cleared).toBe(true);
		// Every handler clear is attempted despite the metadata throw.
		expect(attempts).toEqual([...ACTIONS]);
		// Supported actions are cleared.
		for (const action of ACTIONS) {
			expect(handlers.get(action), action).toBeNull();
		}
	});

	it('retains false return for null sessions without throwing', () => {
		expect(() => {
			expect(clearMediaSession({ session: null })).toBe(false);
		}).not.toThrow();
		expect(() =>
			applyMediaSession(null, { onNext: vi.fn(), onPrevious: vi.fn() }, { session: null })
		).not.toThrow();
		expect(
			applyMediaSession(null, { onNext: vi.fn(), onPrevious: vi.fn() }, { session: null })
		).toBe(false);
	});

	it('isolates handler-clear failures when metadata clear also fails', () => {
		const attempts: Action[] = [];
		const { session, handlers } = nullRejectingSession(attempts, 'previoustrack');
		const options = { session, MetadataCtor: FakeMetadata, baseUrl: BASE };

		applyMediaSession(DEMO_TRACKS[1]!, handlerSet(), options);
		attempts.length = 0;

		let cleared: boolean | undefined;
		expect(() => {
			cleared = clearMediaSession(options);
		}).not.toThrow();

		expect(cleared).toBe(true);
		expect(attempts).toEqual([...ACTIONS]);
		// Survivors land as null despite both failure modes.
		expect(handlers.get('nexttrack')).toBeNull();
		expect(handlers.get('play')).toBeNull();
		expect(handlers.get('pause')).toBeNull();
	});
});
