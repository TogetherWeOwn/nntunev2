import { afterEach, expect, test, vi } from 'vitest';
import { get } from 'svelte/store';
import { createPlayerSession } from './session.js';
import { createQueue, selectTrack } from './queue.js';

const QUEUE = createQueue([
	{ id: 'one', title: 'First', artist: '', streamUrl: '' },
	{ id: 'two', title: 'Second', artist: '', streamUrl: '' }
]);

function session() {
	const player = createPlayerSession({ flush: () => Promise.resolve() });
	player.start(null);
	player.setQueue(QUEUE);
	return player;
}

function clipboard() {
	const writes: { resolve: () => void; reject: (error: Error) => void }[] = [];
	const writeText = vi.fn(() => new Promise<void>((resolve, reject) => writes.push({ resolve, reject })));
	vi.stubGlobal('navigator', { clipboard: { writeText } });
	return { writes, writeText };
}

afterEach(() => vi.unstubAllGlobals());

for (const boundary of ['edit', 'selection', 'away-and-back', 'auth', 'destroy'] as const) {
	for (const rejected of [false, true]) {
		test(`${boundary} synchronously fences a pending export ${rejected ? 'rejection' : 'resolution'}`, async () => {
			const { writes } = clipboard();
			const player = session();
			const pending = player.queueExport.copy();
			if (boundary === 'edit') player.editQueue({ type: 'remove', index: 1 });
			else if (boundary === 'selection') player.setQueue(selectTrack(QUEUE, 1));
			else if (boundary === 'away-and-back') {
				player.setQueue(selectTrack(QUEUE, 1));
				player.setQueue(QUEUE);
			} else if (boundary === 'auth') {
				player.resetAtAuthBoundary();
				player.setQueue(QUEUE);
			} else player.destroy();
			const before = get(player.queueExport);
			if (rejected) writes[0].reject(new Error('Denied'));
			else writes[0].resolve();
			await pending;
			expect(get(player.queueExport)).toEqual(before);
			expect(before).toEqual({ status: '', fallback: null });
			player.destroy();
		});
	}
}

test('presentation subscriptions, filter text, repeat and a queue no-op do not retire export intent', async () => {
	const { writes, writeText } = clipboard();
	const player = session();
	const off = player.queueExport.subscribe(() => {});
	const pending = player.queueExport.copy();
	off();
	player.setQueueFilter('Second');
	player.setRepeat('queue');
	player.setQueue(QUEUE);
	writes[0].resolve();
	await pending;
	expect(writeText).toHaveBeenCalledExactlyOnceWith('1. First\n2. Second');
	expect(get(player.queueExport)).toEqual({ status: 'Copied queue as text.', fallback: null });
	const unmount = player.queueExport.subscribe(() => {});
	unmount();
	expect(get(player.queueExport).status).toBe('Copied queue as text.');
	player.destroy();
});

test('a layout cannot publish export feedback into another layout', async () => {
	const { writes } = clipboard();
	const first = session();
	const second = session();
	const pending = first.queueExport.copy();
	second.setQueueFilter('First');
	writes[0].reject(new Error('Denied'));
	await pending;
	expect(get(first.queueExport).fallback).toBe('1. First\n2. Second');
	expect(get(second.queueExport)).toEqual({ status: '', fallback: null });
	first.destroy();
	second.destroy();
});

test('inactive or destroyed sessions never access clipboard or create a download', async () => {
	const { writeText } = clipboard();
	const createElement = vi.fn();
	vi.stubGlobal('document', { createElement });
	const player = createPlayerSession({ flush: () => Promise.resolve() });
	player.setQueue(QUEUE);
	await player.queueExport.copy();
	player.queueExport.download();
	player.start(null);
	player.destroy();
	player.setQueue(QUEUE);
	await player.queueExport.copy();
	player.queueExport.download();
	expect(writeText).not.toHaveBeenCalled();
	expect(createElement).not.toHaveBeenCalled();
});
