import { afterEach, describe, expect, it, vi } from 'vitest';
import { createPlayerSession } from './session.js';
import { attachFavoritesObserver } from '../../../tests/helpers/favorites-observer.js';

afterEach(() => vi.unstubAllGlobals());

describe('test-only Favorites owner observer', () => {
	it('copies the actual owner, follows real transitions, and never exposes mutations', () => {
		vi.stubGlobal('window', {});
		const player = createPlayerSession({ flush: async () => {} });
		const detach = attachFavoritesObserver(player);
		const binding = window.__favoritesQueueOwner!;
		expect(Object.keys(binding)).toEqual(['snapshot', 'selectionVersion']);
		expect(Object.isFrozen(binding)).toBe(true);
		expect(Object.getOwnPropertyDescriptor(binding, 'selectionVersion')?.set).toBeUndefined();
		expect(binding.selectionVersion).toBe(0);
		player.setQueue({ tracks: [{ id: 'a', title: 'Alpha', artist: '', streamUrl: '' }, { id: 'b', title: 'Bravo', artist: '', streamUrl: '' }], currentIndex: 0 });
		const first = binding.snapshot;
		expect(first).toEqual({ ids: ['a', 'b'], currentIndex: 0, currentId: 'a', selectedTitle: 'Alpha' });
		expect(binding.selectionVersion).toBe(1);
		first.ids.reverse();
		expect(binding.snapshot.ids).toEqual(['a', 'b']);
		player.setQueue({ ...player.snapshot().queue, currentIndex: 1 });
		expect(binding.snapshot).toEqual({ ids: ['a', 'b'], currentIndex: 1, currentId: 'b', selectedTitle: 'Bravo' });
		expect(binding.selectionVersion).toBe(2);
		player.setQueue({ ...player.snapshot().queue, tracks: [...player.snapshot().queue.tracks, { id: 'c', title: 'Charlie', artist: '', streamUrl: '' }] });
		expect(binding.selectionVersion).toBe(2);
		player.setRepeat('queue');
		expect(binding.selectionVersion).toBe(2);
		detach();
		expect(window.__favoritesQueueOwner).toBeUndefined();
		expect(() => binding.snapshot).toThrow(/Disposed/);
		expect(() => binding.selectionVersion).toThrow(/Disposed/);
		player.destroy();
	});

	it('refuses duplicate owners and unsubscribes on teardown', () => {
		vi.stubGlobal('window', {});
		const player = createPlayerSession({ flush: async () => {} });
		const unsubscribe = vi.fn();
		const subscribe = vi.spyOn(player, 'subscribe').mockImplementation((run) => {
			const real = createPlayerSession({ flush: async () => {} });
			const stop = real.subscribe(run);
			stop();
			real.destroy();
			return unsubscribe;
		});
		const detach = attachFavoritesObserver(player);
		expect(() => attachFavoritesObserver(player)).toThrow(/Duplicate/);
		expect(subscribe).toHaveBeenCalledTimes(1);
		detach();
		expect(unsubscribe).toHaveBeenCalledOnce();
		player.destroy();
	});
});
