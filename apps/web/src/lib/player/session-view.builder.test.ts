/** Builder-owned tests for layout-local queue view state, not decoder evidence. */
import { describe, expect, it, vi } from 'vitest';
import { createPlayerSession } from './session.js';
import { createQueue } from './queue.js';

function setup() {
	const flush = vi.fn(async () => {});
	const storage = { getItem: vi.fn(() => null), setItem: vi.fn(), removeItem: vi.fn() };
	const session = createPlayerSession({ flush });
	session.start(storage);
	session.setQueue(createQueue([
		{ id: 'a', title: 'Alpha', artist: 'Test artist', streamUrl: '/a' },
		{ id: 'b', title: 'Beta', artist: 'Test artist', streamUrl: '/b' }
	]));
	return { session, storage, flush };
}

describe('persistent queue view state', () => {
	it('retains density through view remount, queue edits and auth without media or storage work', () => {
		const { session, storage, flush } = setup();
		const before = session.snapshot();
		session.setQueueDensity('compact');
		expect(session.snapshot()).toEqual({ ...before, queueDensity: 'compact' });
		const compact = session.snapshot();
		session.setQueueDensity('compact');
		expect(session.snapshot()).toBe(compact);
		const values: string[] = [];
		const off = session.subscribe((state) => values.push(state.queueDensity));
		off();
		session.editQueue({ type: 'clear' });
		expect(session.snapshot().queueDensity).toBe('compact');
		session.resetAtAuthBoundary();
		expect(session.snapshot().queueDensity).toBe('compact');
		expect(values).toEqual(['compact']);
		expect(storage.setItem).not.toHaveBeenCalled();
		expect(flush).not.toHaveBeenCalled();
	});

	it('keeps exact query text across subscriber removal and remount without queue or storage work', () => {
		const { session, storage, flush } = setup();
		const before = session.snapshot();
		const unsubscribe = session.subscribe(() => {});
		session.setQueueFilter('  BeTA  ');
		unsubscribe();
		let query = '';
		const stop = session.subscribe((s) => { query = s.queueFilterQuery; });
		expect(query).toBe('  BeTA  ');
		expect(session.snapshot()).toMatchObject({
			queue: before.queue, audio: before.audio, transport: before.transport,
			repeat: before.repeat, queueConfirmGeneration: 0		});
		expect(session.snapshot().queue).toBe(before.queue);
		expect(flush).not.toHaveBeenCalled();
		expect(storage.setItem).not.toHaveBeenCalled();
		expect(storage.removeItem).not.toHaveBeenCalled();
		stop();
	});

	it('setting the same query is silent and clearing it does not edit queue or feedback', () => {
		const { session } = setup();
		session.setQueueFeedback('Kept action outcome.', true);
		session.setQueueFilter('Alpha');
		const before = session.snapshot();
		const observer = vi.fn();
		const stop = session.subscribe(observer);
		observer.mockClear();
		session.setQueueFilter('Alpha');
		expect(observer).not.toHaveBeenCalled();
		session.setQueueFilter('');
		expect(observer).toHaveBeenCalledTimes(1);
		expect(session.snapshot()).toEqual({ ...before, queueFilterQuery: '' });
		stop();
	});

	it('routine status suspension and release preserve queue, feedback and transport', () => {
		const { session, storage, flush } = setup();
		session.setQueueFeedback('Selected Alpha');
		const before = session.snapshot();
		session.setQueueLiveRegions(false);
		expect(session.snapshot()).toEqual({ ...before, queueLiveRegions: false });
		const suspended = session.snapshot();
		session.setQueueLiveRegions(false);
		expect(session.snapshot()).toBe(suspended);
		session.setQueueLiveRegions(true);
		expect(session.snapshot()).toEqual(before);
		expect(flush).not.toHaveBeenCalled();
		expect(storage.setItem).not.toHaveBeenCalled();
	});

	it('normal queue edits and clearing preserve the filter and invalidation counters', () => {
		const { session } = setup();
		session.setQueueFilter('Beta');
		session.invalidateQueueConsent();
		session.editQueue({ type: 'move', from: 1, to: 0 });
		expect(session.snapshot().queueFilterQuery).toBe('Beta');
		session.editQueue({ type: 'clear' });
		expect(session.snapshot().queue.tracks).toEqual([]);
		expect(session.snapshot()).toMatchObject({ queueFilterQuery: 'Beta', queueConfirmGeneration: 1 });
	});

	it('each explicit implicit-boundary notification advances consent without queue mutation', () => {
		const { session, storage, flush } = setup();
		const before = session.snapshot();
		session.invalidateQueueConsent();
		session.invalidateQueueConsent();
		expect(session.snapshot()).toEqual({ ...before, queueConfirmGeneration: 2 });
		expect(session.snapshot().queue).toBe(before.queue);
		expect(flush).not.toHaveBeenCalled();
		expect(storage.setItem).not.toHaveBeenCalled();
	});

	// Export invalidation is exercised through the real controller in session-export.builder.test.ts.
	it.each(['resetAtAuthBoundary', 'destroy'] as const)('%s clears text and advances consent monotonically', (boundary) => {
		const { session } = setup();
		session.setQueueFilter('Previous account');
		session.setQueueLiveRegions(false);
		session.invalidateQueueConsent();
		session[boundary]();
		expect(session.snapshot()).toMatchObject({ queueFilterQuery: '', queueLiveRegions: true, queueConfirmGeneration: 2 });
		session[boundary]();
		expect(session.snapshot()).toMatchObject({ queueFilterQuery: '', queueConfirmGeneration: 3 });
	});

	it.each(['resetAtAuthBoundary', 'destroy'] as const)('%s clears view data without resetting saved repeat or conflating action identity', (boundary) => {
		const { session } = setup();
		session.setRepeat('one');
		session.initializeFavorites();
		session.toggleFavorite('a');
		session.setQueueFeedback('First outcome.', true);
		session.setQueueFeedback('Second outcome.', true);
		const old = session.snapshot();
		session[boundary]();
		const cleared = session.snapshot();
		expect(cleared).toMatchObject({
			favorites: [], audio: null, repeat: 'one', announcement: '',
			queueFeedback: '', queueFeedbackExposed: false,
			queueShareStatus: '', queueShareFallbackUrl: null,
			restartAvailable: false, restartStatus: '',
			queueConfirmGeneration: old.queueConfirmGeneration + 1,
			queueFeedbackVersion: boundary === 'destroy' ? 2 : 0
		});
		expect(cleared.queue.tracks).toEqual([]);
		expect(cleared.favorites).not.toBe(old.favorites);
		expect(cleared.queue).not.toBe(old.queue);
	});

	it('auth reset keeps the new account usable without reloading old view state', () => {
		const { session } = setup();
		session.setQueueFilter('Old');
		session.resetAtAuthBoundary();
		session.setQueueFilter('New');
		session.invalidateQueueConsent();
		expect(session.snapshot()).toMatchObject({ queueFilterQuery: 'New', queueConfirmGeneration: 2 });
	});

	it('before start and after destroy, view callbacks do not publish', () => {
		const session = createPlayerSession({ flush: async () => {} });
		for (const boundary of [() => {}, session.destroy]) {
			boundary();
			const before = session.snapshot();
			session.setQueueFilter('stale');
			session.setQueueLiveRegions(false);
			session.invalidateQueueConsent();
			expect(session.snapshot()).toBe(before);
		}
	});

	it('separate layout instances never share query text or invalidation counters', () => {
		const { session: first } = setup();
		const { session: second } = setup();
		first.setQueueFilter('First');
		first.invalidateQueueConsent();
		first.resetAtAuthBoundary();
		first.setQueueFilter('First after reset');
		expect(second.snapshot()).toMatchObject({ queueFilterQuery: '', queueConfirmGeneration: 0 });
		expect(first.snapshot()).toMatchObject({ queueFilterQuery: 'First after reset', queueConfirmGeneration: 2 });
	});
});
