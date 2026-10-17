import { describe, expect, it, vi } from 'vitest';
import { ARTWORK_PLACEHOLDER_SRC } from './artwork.js';
import {
	applyMediaSession,
	buildMediaMetadataInit,
	type MediaMetadataInit,
	type MediaSessionLike
} from './mediaSession.js';
import type { Track } from './queue.js';

/**
 * Acceptance: malformed artwork metadata must never break Media Session
 * controls. A track whose artworkUrl cannot be parsed as a URL (or whose
 * MediaMetadata construction fails) still gets the current track's
 * next/previous/play/pause handlers, and artwork falls back to the shared
 * placeholder instead of throwing out of metadata construction.
 */

const BASE = 'https://example.test';
const PLACEHOLDER_ABSOLUTE = `${BASE}${ARTWORK_PLACEHOLDER_SRC}`;

class FakeMetadata {
	constructor(readonly init: MediaMetadataInit) {}
}

class ThrowingMetadata {
	constructor() {
		throw new Error('MediaMetadata construction failed');
	}
}

function fakeSession() {
	const handlers = new Map<string, (() => void) | null>();
	const session: MediaSessionLike = {
		metadata: null,
		setActionHandler(action, handler) {
			handlers.set(action, handler);
		}
	};
	return { session, handlers };
}

function trackWith(artworkUrl: string | null, overrides: Partial<Track> = {}): Track {
	return {
		id: 'synthetic',
		title: 'Synthetic Track',
		artist: 'Fixture Band',
		streamUrl: '/api/demo/audio?track=synthetic',
		artworkUrl,
		...overrides
	};
}

function handlerSet() {
	return {
		onNext: vi.fn(),
		onPrevious: vi.fn(),
		onPlay: vi.fn(),
		onPause: vi.fn()
	};
}

const ACTIONS = ['nexttrack', 'previoustrack', 'play', 'pause'] as const;

describe('buildMediaMetadataInit with malformed artwork URLs', () => {
	it.each(['https://[', 'http://[::1', 'http://]', 'https://exa mple.com/art.jpg'])(
		'falls back to the placeholder instead of throwing for %s',
		(artworkUrl) => {
			const track = trackWith(artworkUrl);
			let init: MediaMetadataInit | undefined;
			expect(() => {
				init = buildMediaMetadataInit(track, BASE);
			}).not.toThrow();
			expect(init?.title).toBe('Synthetic Track');
			expect(init?.artist).toBe('Fixture Band');
			expect(init?.artwork).toEqual([{ src: PLACEHOLDER_ABSOLUTE, sizes: '512x512' }]);
		}
	);

	it('preserves a valid absolute artwork URL (positive control)', () => {
		const init = buildMediaMetadataInit(trackWith('https://cdn.example/art/abc.jpg'), BASE);
		expect(init.artwork).toEqual([{ src: 'https://cdn.example/art/abc.jpg', sizes: '512x512' }]);
	});

	it('preserves and absolutizes a valid relative artwork URL (positive control)', () => {
		const init = buildMediaMetadataInit(trackWith('/api/demo/artwork'), BASE);
		expect(init.artwork).toEqual([{ src: `${BASE}/api/demo/artwork`, sizes: '512x512' }]);
	});

	it('preserves a data: artwork URL (positive control)', () => {
		const init = buildMediaMetadataInit(trackWith('data:image/svg+xml;base64,AAA'), BASE);
		expect(init.artwork).toEqual([{ src: 'data:image/svg+xml;base64,AAA', sizes: '512x512' }]);
	});

	it('keeps missing-artwork placeholder behavior (positive control)', () => {
		const init = buildMediaMetadataInit(trackWith(null), BASE);
		expect(init.artwork).toEqual([{ src: PLACEHOLDER_ABSOLUTE, sizes: '512x512' }]);
	});
});

describe('applyMediaSession with malformed artwork', () => {
	it('does not throw, uses placeholder artwork, and wires the new track handlers', () => {
		const { session, handlers } = fakeSession();
		const next = handlerSet();

		let applied: boolean | undefined;
		expect(() => {
			applied = applyMediaSession(trackWith('https://['), next, {
				session,
				MetadataCtor: FakeMetadata,
				baseUrl: BASE
			});
		}).not.toThrow();

		expect(applied).toBe(true);
		expect(session.metadata).toBeInstanceOf(FakeMetadata);
		expect((session.metadata as FakeMetadata).init.artwork).toEqual([
			{ src: PLACEHOLDER_ABSOLUTE, sizes: '512x512' }
		]);
		for (const action of ACTIONS) handlers.get(action)?.();
		expect(next.onNext).toHaveBeenCalledOnce();
		expect(next.onPrevious).toHaveBeenCalledOnce();
		expect(next.onPlay).toHaveBeenCalledOnce();
		expect(next.onPause).toHaveBeenCalledOnce();
	});

	it('replaces prior callbacks when the new track artwork is malformed', () => {
		const { session, handlers } = fakeSession();
		const options = { session, MetadataCtor: FakeMetadata, baseUrl: BASE };
		const first = handlerSet();
		const second = handlerSet();

		applyMediaSession(trackWith('/api/demo/artwork', { id: 'first', title: 'First' }), first, options);
		expect(() =>
			applyMediaSession(trackWith('https://[', { id: 'second', title: 'Second' }), second, options)
		).not.toThrow();

		expect((session.metadata as FakeMetadata).init.title).toBe('Second');
		for (const action of ACTIONS) handlers.get(action)?.();
		// Only the latest track's callbacks may be active.
		expect(second.onNext).toHaveBeenCalledOnce();
		expect(second.onPrevious).toHaveBeenCalledOnce();
		expect(second.onPlay).toHaveBeenCalledOnce();
		expect(second.onPause).toHaveBeenCalledOnce();
		expect(first.onNext).not.toHaveBeenCalled();
		expect(first.onPrevious).not.toHaveBeenCalled();
		expect(first.onPlay).not.toHaveBeenCalled();
		expect(first.onPause).not.toHaveBeenCalled();
	});
});

describe('applyMediaSession when MediaMetadata construction fails', () => {
	it('still wires handlers on first apply when the constructor throws', () => {
		const { session, handlers } = fakeSession();
		const only = handlerSet();

		expect(() =>
			applyMediaSession(trackWith('/api/demo/artwork'), only, {
				session,
				MetadataCtor: ThrowingMetadata,
				baseUrl: BASE
			})
		).not.toThrow();

		for (const action of ACTIONS) {
			expect(handlers.get(action), action).toBeTypeOf('function');
			handlers.get(action)?.();
		}
		expect(only.onNext).toHaveBeenCalledOnce();
		expect(only.onPrevious).toHaveBeenCalledOnce();
		expect(only.onPlay).toHaveBeenCalledOnce();
		expect(only.onPause).toHaveBeenCalledOnce();
	});

	it('updates to the latest callbacks and does not leave prior callbacks active', () => {
		const { session, handlers } = fakeSession();
		const first = handlerSet();
		const second = handlerSet();

		applyMediaSession(trackWith('/api/demo/artwork', { id: 'first', title: 'First' }), first, {
			session,
			MetadataCtor: FakeMetadata,
			baseUrl: BASE
		});
		expect(() =>
			applyMediaSession(trackWith('/api/demo/artwork', { id: 'second', title: 'Second' }), second, {
				session,
				MetadataCtor: ThrowingMetadata,
				baseUrl: BASE
			})
		).not.toThrow();

		for (const action of ACTIONS) handlers.get(action)?.();
		expect(second.onNext).toHaveBeenCalledOnce();
		expect(second.onPrevious).toHaveBeenCalledOnce();
		expect(second.onPlay).toHaveBeenCalledOnce();
		expect(second.onPause).toHaveBeenCalledOnce();
		expect(first.onNext).not.toHaveBeenCalled();
		expect(first.onPrevious).not.toHaveBeenCalled();
		expect(first.onPlay).not.toHaveBeenCalled();
		expect(first.onPause).not.toHaveBeenCalled();
	});

	it('recovers on the next good track after a construction failure', () => {
		const { session, handlers } = fakeSession();
		const failing = handlerSet();
		const good = handlerSet();

		expect(() =>
			applyMediaSession(trackWith('/api/demo/artwork', { id: 'bad', title: 'Bad' }), failing, {
				session,
				MetadataCtor: ThrowingMetadata,
				baseUrl: BASE
			})
		).not.toThrow();
		applyMediaSession(trackWith('/api/demo/artwork', { id: 'good', title: 'Good' }), good, {
			session,
			MetadataCtor: FakeMetadata,
			baseUrl: BASE
		});

		expect((session.metadata as FakeMetadata).init.title).toBe('Good');
		handlers.get('nexttrack')?.();
		expect(good.onNext).toHaveBeenCalledOnce();
		expect(failing.onNext).not.toHaveBeenCalled();
	});

	it('null-track teardown still clears metadata and handlers after a failure', () => {
		const { session, handlers } = fakeSession();
		const first = handlerSet();
		const failing = handlerSet();

		applyMediaSession(trackWith('/api/demo/artwork', { id: 'first' }), first, {
			session,
			MetadataCtor: FakeMetadata,
			baseUrl: BASE
		});
		expect(session.metadata).not.toBeNull();
		expect(() =>
			applyMediaSession(trackWith('https://[', { id: 'bad' }), failing, {
				session,
				MetadataCtor: ThrowingMetadata,
				baseUrl: BASE
			})
		).not.toThrow();

		applyMediaSession(null, failing, { session, MetadataCtor: FakeMetadata, baseUrl: BASE });
		expect(session.metadata).toBeNull();
		for (const action of ACTIONS) expect(handlers.get(action)).toBeNull();
	});
});

describe('applyMediaSession preserved behavior around artwork failures', () => {
	it('reports false and throws nothing when unsupported, even with malformed artwork', () => {
		expect(() =>
			applyMediaSession(trackWith('https://['), handlerSet(), { session: null })
		).not.toThrow();
		expect(applyMediaSession(trackWith('https://['), handlerSet(), { session: null })).toBe(false);
	});

	it('keeps per-handler exception isolation when artwork is malformed', () => {
		const handlers = new Map<string, (() => void) | null>();
		const throwingSession: MediaSessionLike = {
			metadata: null,
			setActionHandler(action, handler) {
				if (action === 'pause') throw new Error('unsupported');
				handlers.set(action, handler);
			}
		};
		const callbacks = handlerSet();

		let applied: boolean | undefined;
		expect(() => {
			applied = applyMediaSession(trackWith('https://['), callbacks, {
				session: throwingSession,
				MetadataCtor: FakeMetadata,
				baseUrl: BASE
			});
		}).not.toThrow();

		expect(applied).toBe(true);
		expect((throwingSession.metadata as FakeMetadata).init.artwork).toEqual([
			{ src: PLACEHOLDER_ABSOLUTE, sizes: '512x512' }
		]);
		expect(handlers.get('nexttrack')).toBeTypeOf('function');
		expect(handlers.get('previoustrack')).toBeTypeOf('function');
		handlers.get('nexttrack')?.();
		expect(callbacks.onNext).toHaveBeenCalledOnce();
	});
});
