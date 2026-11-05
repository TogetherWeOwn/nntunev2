/** The production session, not a route facade, owns queued-copy lifecycle. */
import { afterEach, describe, expect, it, vi } from 'vitest';
import { createPlayerSession } from './session.js';
import { createQueue, moveTrack, removeTrack, selectTrack } from './queue.js';

const tracks = ['a', 'b', 'c'].map((id) => ({ id, title: `Title ${id}`, artist: '', streamUrl: `/${id}` }));
const sessions: ReturnType<typeof createPlayerSession>[] = [];
function setup() {
	const writes: { url: string; resolve: () => void; reject: () => void }[] = [];
	const writeText = vi.fn((url: string) => new Promise<void>((resolve, reject) => {
		writes.push({ url, resolve, reject: () => reject(new Error('Refused')) });
	}));
	vi.stubGlobal('window', { location: { origin: 'https://example.test' } });
	vi.stubGlobal('navigator', { clipboard: { writeText } });
	const player = createPlayerSession({ flush: async () => {} });
	sessions.push(player);
	player.setQueue(createQueue(tracks));
	player.start(null);
	return { player, writes, writeText };
}
function expectSilent(player: ReturnType<typeof createPlayerSession>) {
	expect(player.snapshot().queueShareStatus).toBe('');
	expect(player.snapshot().queueShareFallbackUrl).toBeNull();
}
afterEach(() => {
	for (const player of sessions.splice(0)) player.destroy();
	vi.unstubAllGlobals();
});

describe('session queued-row sharing', () => {
	it('does nothing before start or after destroy', async () => {
		const { player, writes } = setup();
		player.destroy();
		player.setQueue(createQueue(tracks));
		await player.copyQueuedTrackLink('a');
		expect(writes).toHaveLength(0);
		expectSilent(player);
		const fresh = createPlayerSession({ flush: async () => {} });
		fresh.setQueue(createQueue(tracks));
		await fresh.copyQueuedTrackLink('a');
		expect(writes).toHaveLength(0);
		fresh.destroy();
	});

	it.each([false, true])('auth reset fences a pending %s rejection, even if the id is re-added', async (reject) => {
		const { player, writes } = setup();
		const pending = player.copyQueuedTrackLink('b');
		player.resetAtAuthBoundary();
		player.setQueue(createQueue(tracks));
		if (reject) writes[0].reject(); else writes[0].resolve();
		await pending;
		expectSilent(player);
	});

	it.each([false, true])('destroy fences a pending %s rejection', async (reject) => {
		const { player, writes } = setup();
		const pending = player.copyQueuedTrackLink('b');
		player.destroy();
		if (reject) writes[0].reject(); else writes[0].resolve();
		await pending;
		expectSilent(player);
	});

	it('clears completed feedback on real edits but leaves it on no-op selection', async () => {
		const { player, writes } = setup();
		const pending = player.copyQueuedTrackLink('b');
		writes[0].resolve();
		await pending;
		expect(player.snapshot().queueShareStatus).toBe('Copied link for Title b.');
		expect(player.editQueue({ type: 'select', index: 0 })).toBe(false);
		expect(player.snapshot().queueShareStatus).toBe('Copied link for Title b.');
		player.editQueue({ type: 'move', from: 2, to: 1 });
		expectSilent(player);
	});

	it('clears an existing fallback at a selection change and at auth reset', async () => {
		const { player } = setup();
		vi.stubGlobal('navigator', {});
		await player.copyQueuedTrackLink('b');
		expect(player.snapshot().queueShareFallbackUrl).toBe('https://example.test/library?track=b&t=0');
		player.setQueue(selectTrack(player.snapshot().queue, 1));
		expectSilent(player);
		await player.copyQueuedTrackLink('a');
		player.resetAtAuthBoundary();
		expectSilent(player);
	});

	it.each([false, true])('a pending %s rejection rechecks row membership against the live queue', async (reject) => {
		const { player, writes } = setup();
		const pending = player.copyQueuedTrackLink('b');
		player.setQueue(removeTrack(player.snapshot().queue, 1));
		if (reject) writes[0].reject(); else writes[0].resolve();
		await pending;
		expect(player.snapshot().queueShareStatus).toBe('Link unavailable — Title b is no longer in the queue.');
		expect(player.snapshot().queueShareFallbackUrl).toBeNull();
	});

	it('keeps a pending copy across rearrangement and keeps action feedback independent', async () => {
		const { player, writes } = setup();
		const pending = player.copyQueuedTrackLink('b');
		player.setQueue(moveTrack(player.snapshot().queue, 1, 2));
		player.setQueueFeedback('Independent queue action');
		writes[0].resolve();
		await pending;
		expect(player.snapshot().queueShareStatus).toBe('Copied link for Title b.');
		expect(player.snapshot().queueFeedback).toBe('Independent queue action');
	});

	it('an auth transition during clipboard access does not write the previous account link', async () => {
		const { player, writeText } = setup();
		vi.stubGlobal('navigator', { get clipboard() { player.resetAtAuthBoundary(); return { writeText }; } });
		await player.copyQueuedTrackLink('b');
		expect(writeText).not.toHaveBeenCalled();
		expectSilent(player);
	});
});
