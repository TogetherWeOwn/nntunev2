/** Builder-owned current-track Favorites tests; queue-row focus remains separate. */
import { describe, expect, it } from 'vitest';
import { createPlayerSession } from './session.js';
import { createQueue, type Track } from './queue.js';
import { LIBRARY_AUTH_CONTEXT_KEY, LIBRARY_FAVORITES_KEY } from '../library/cache.js';

const tracks: Track[] = [
	{ id: 'a', title: 'Track A', artist: 'Artist', streamUrl: '/a' },
	{ id: 'b', title: 'Track B', artist: 'Artist', streamUrl: '/b' }
];

function setup() {
	const values = new Map<string, string>([[LIBRARY_AUTH_CONTEXT_KEY, 'current-favorite']]);
	let refuseWrites = false;
	const storage = {
		getItem: (key: string) => values.get(key) ?? null,
		setItem: (key: string, value: string) => {
			if (refuseWrites) throw new Error('quota');
			values.set(key, value);
		},
		removeItem: (key: string) => { values.delete(key); }
	};
	const player = createPlayerSession({ flush: async () => {} });
	player.start(storage, storage);
	player.setQueue(createQueue(tracks));
	return { player, values, refuseWrites: () => { refuseWrites = true; } };
}

const storedKey = `${LIBRARY_FAVORITES_KEY}.current-favorite`;

describe('current-track Favorites action', () => {
	it('uses the same membership, preserves hidden favorites and announces without queue-row aftermath', () => {
		const { player, values } = setup();
		values.set(storedKey, JSON.stringify(['hidden', 'b']));
		player.initializeFavorites();
		let aftermath = 0;
		player.onQueuedFavorite(() => { aftermath++; });
		const before = player.snapshot();
		expect(player.toggleCurrentFavorite()).toBe(true);
		expect(player.snapshot().favorites).toEqual(['hidden', 'b', 'a']);
		expect(JSON.parse(values.get(storedKey)!)).toEqual(['hidden', 'b', 'a']);
		expect(player.snapshot().queueFeedback).toBe('Saved Track A to Favorites.');
		expect(player.toggleCurrentFavorite()).toBe(true);
		expect(player.snapshot().favorites).toEqual(['hidden', 'b']);
		expect(player.snapshot().queueFeedback).toBe('Removed Track A from Favorites.');
		expect(aftermath).toBe(0);
		for (const key of ['queue', 'audio', 'transport', 'repeat', 'announcement'] as const) {
			expect(player.snapshot()[key]).toBe(before[key]);
		}
		expect(player.toggleQueuedFavorite('a')).toBe(true);
		expect(player.snapshot().favorites).toContain('a');
		expect(aftermath).toBe(1);
		player.destroy();
	});

	it('resolves the current selection at activation, including an unavailable track', () => {
		const { player } = setup();
		player.initializeFavorites();
		player.toggleCurrentFavorite();
		player.setQueue({ ...createQueue(tracks), currentIndex: 1 });
		player.toggleCurrentFavorite();
		expect(player.snapshot().favorites).toEqual(['a', 'b']);
		expect(player.snapshot().queueFeedback).toBe('Saved Track B to Favorites.');
		player.setQueue(createQueue([{ id: 'metadata', title: '  ', artist: '', streamUrl: '' }]));
		player.toggleCurrentFavorite();
		expect(player.snapshot().favorites).toEqual(['a', 'b', 'metadata']);
		expect(player.snapshot().queueFeedback).toBe('Saved metadata to Favorites.');
		player.destroy();
	});

	it('retains memory after refused writes instead of reloading stale persisted membership', () => {
		const { player, values, refuseWrites } = setup();
		values.set(storedKey, JSON.stringify(['b']));
		player.initializeFavorites();
		refuseWrites();
		expect(player.toggleCurrentFavorite()).toBe(true);
		player.initializeFavorites();
		expect(player.snapshot().favorites).toEqual(['b', 'a']);
		expect(JSON.parse(values.get(storedKey)!)).toEqual(['b']);
		expect(player.toggleCurrentFavorite()).toBe(true);
		expect(player.snapshot().favorites).toEqual(['b']);
		player.destroy();
	});

	it('is inert before bootstrap, after auth reset, and after destroy even with retained selection', () => {
		const { player, values } = setup();
		let before = player.snapshot();
		expect(player.toggleCurrentFavorite()).toBe(false);
		expect(player.snapshot()).toBe(before);
		player.initializeFavorites();
		player.toggleCurrentFavorite();
		player.resetAtAuthBoundary();
		player.setQueue(createQueue(tracks));
		before = player.snapshot();
		expect(player.toggleCurrentFavorite()).toBe(false);
		expect(player.snapshot()).toBe(before);
		expect(values.has(storedKey)).toBe(false);
		player.initializeFavorites();
		player.destroy();
		player.setQueue(createQueue(tracks));
		before = player.snapshot();
		expect(player.toggleCurrentFavorite()).toBe(false);
		expect(player.snapshot()).toBe(before);
	});

	it('does not announce, write or publish with an empty queue', () => {
		const { player, values } = setup();
		player.initializeFavorites();
		player.setQueue(createQueue([]));
		const before = player.snapshot();
		const saved = [...values];
		expect(player.toggleCurrentFavorite()).toBe(false);
		expect(player.snapshot()).toBe(before);
		expect([...values]).toEqual(saved);
		player.destroy();
	});
});
