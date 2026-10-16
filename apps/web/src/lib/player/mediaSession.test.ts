import { describe, expect, it, vi } from 'vitest';
import { ARTWORK_PLACEHOLDER_SRC } from './artwork.js';
import {
	applyMediaSession,
	buildMediaMetadataInit,
	clearMediaSession,
	type MediaMetadataInit,
	type MediaSessionLike
} from './mediaSession.js';
import { DEMO_TRACKS, type Track } from './queue.js';

class FakeMetadata {
	constructor(readonly init: MediaMetadataInit) {}
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

describe('buildMediaMetadataInit', () => {
	it('carries title/artist and absolute artwork for the current track', () => {
		const track: Track = {
			id: 'x',
			title: 'Second Signal',
			artist: 'Fixture Band',
			streamUrl: '/api/demo/audio?track=x',
			artworkUrl: '/api/demo/artwork'
		};
		const init = buildMediaMetadataInit(track, 'https://example.test');
		expect(init.title).toBe('Second Signal');
		expect(init.artist).toBe('Fixture Band');
		expect(init.artwork).toEqual([{ src: 'https://example.test/api/demo/artwork', sizes: '512x512' }]);
	});

	it('reuses the artwork fallback (placeholder) when a track has no art', () => {
		// DEMO_TRACKS[0] deliberately has no artwork: resolveArtwork must
		// supply the placeholder instead of an empty MediaSession image.
		const init = buildMediaMetadataInit(DEMO_TRACKS[0]!, 'https://example.test');
		expect(init.artwork?.[0]?.src).toBe(`https://example.test${ARTWORK_PLACEHOLDER_SRC}`);
	});
});

describe('applyMediaSession', () => {
	it('sets metadata and wires OS next/prev/play/pause handlers', () => {
		const { session, handlers } = fakeSession();
		const onNext = vi.fn();
		const onPrevious = vi.fn();
		const onPlay = vi.fn();
		const onPause = vi.fn();

		const applied = applyMediaSession(DEMO_TRACKS[1]!, { onNext, onPrevious, onPlay, onPause }, {
			session,
			MetadataCtor: FakeMetadata,
			baseUrl: 'https://example.test'
		});

		expect(applied).toBe(true);
		expect(session.metadata).toBeInstanceOf(FakeMetadata);
		expect((session.metadata as FakeMetadata).init.title).toBe(DEMO_TRACKS[1]?.title);
		handlers.get('nexttrack')?.();
		handlers.get('previoustrack')?.();
		handlers.get('play')?.();
		handlers.get('pause')?.();
		expect(onNext).toHaveBeenCalledOnce();
		expect(onPrevious).toHaveBeenCalledOnce();
		expect(onPlay).toHaveBeenCalledOnce();
		expect(onPause).toHaveBeenCalledOnce();
	});

	it('clears metadata and handlers when the queue empties', () => {
		const { session, handlers } = fakeSession();
		const options = { session, MetadataCtor: FakeMetadata, baseUrl: 'https://example.test' };
		const noop = { onNext: vi.fn(), onPrevious: vi.fn() };
		// Start from a populated session: a fresh fake already has null
		// metadata, which would let a missing clear pass unnoticed.
		applyMediaSession(DEMO_TRACKS[1]!, noop, options);
		expect(session.metadata).toBeInstanceOf(FakeMetadata);
		expect(handlers.get('nexttrack')).toBeTypeOf('function');

		applyMediaSession(null, noop, options);
		expect(session.metadata).toBeNull();
		expect(handlers.get('nexttrack')).toBeNull();
		expect(handlers.get('previoustrack')).toBeNull();
		expect(handlers.get('play')).toBeNull();
		expect(handlers.get('pause')).toBeNull();
	});

	it('clearMediaSession releases a live session (player teardown)', () => {
		const { session, handlers } = fakeSession();
		const options = { session, MetadataCtor: FakeMetadata, baseUrl: 'https://example.test' };
		applyMediaSession(
			DEMO_TRACKS[0]!,
			{ onNext: vi.fn(), onPrevious: vi.fn(), onPlay: vi.fn(), onPause: vi.fn() },
			options
		);
		expect(session.metadata).not.toBeNull();

		expect(clearMediaSession(options)).toBe(true);
		expect(session.metadata).toBeNull();
		for (const action of ['nexttrack', 'previoustrack', 'play', 'pause']) {
			expect(handlers.get(action)).toBeNull();
		}
		expect(clearMediaSession({ session: null })).toBe(false);
	});

	it('reports false and throws nothing when MediaSession is unsupported', () => {
		expect(
			applyMediaSession(DEMO_TRACKS[0]!, { onNext: vi.fn(), onPrevious: vi.fn() }, { session: null })
		).toBe(false);
	});

	it('survives a browser rejecting an individual action handler', () => {
		const { session, handlers } = fakeSession();
		const throwing: MediaSessionLike = {
			metadata: null,
			setActionHandler(action, handler) {
				if (action === 'pause') throw new Error('unsupported');
				handlers.set(action, handler);
			}
		};
		expect(
			applyMediaSession(
				DEMO_TRACKS[0]!,
				{ onNext: vi.fn(), onPrevious: vi.fn(), onPause: vi.fn() },
				{ session: throwing, MetadataCtor: FakeMetadata, baseUrl: 'https://example.test' }
			)
		).toBe(true);
		expect(handlers.has('nexttrack')).toBe(true);
	});
});
