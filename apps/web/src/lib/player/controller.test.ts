import { expect, test, vi } from 'vitest';
import { clearMiniPlayerHint, onMiniPlayerHint, type MiniPlayerHint } from '../shell/mini-player.js';
import { createPlayerController } from './controller.svelte.js';
import { createQueue } from './queue.js';

test('queue commits publish only display metadata and skip same-object Library refreshes', () => {
	const player = createPlayerController(createQueue([]), {
		flush: async () => {}
	});
	const next = {
		tracks: [
			{ id: 'first', title: 'First', artist: 'First Artist', streamUrl: '' },
			{ id: 'selected', title: ' ', artist: '\t', streamUrl: '/test-audio/selected', album: 'Not a hint field' }
		],
		currentIndex: 1
	};
	const seen = vi.fn((_hint: MiniPlayerHint | null) => player.queue.snapshot);
	const stop = onMiniPlayerHint(seen);
	seen.mockClear(); // Skip the store's initial delivery.
	try {
		expect(player.replaceQueue(next)).toBe(true);
		const library = player.queue.prepareLibrary(next.tracks);
		expect(library.next).toBe(next);
		expect(player.commitQueue(library.next, 'replace', { sources: library.sources, isCurrent: () => true })).toBe(true);
		expect(seen.mock.calls).toStrictEqual([[{ trackId: 'selected', title: 'selected', artist: null }]]);
		expect(seen.mock.results[0].value).toBe(next);

		expect(player.commitQueue(createQueue([]), 'clear')).toBe(true);
		expect(seen.mock.calls).toHaveLength(2);
		expect(seen).toHaveBeenLastCalledWith(null);
	} finally {
		stop();
		clearMiniPlayerHint();
	}
});

test('queue feedback preserves replay keys across refresh and replacement', () => {
	const initial = createQueue([]);
	const effects = { flush: async () => {} };
	const player = createPlayerController(initial, effects);
	const other = createPlayerController(initial, effects);
	const text = 'Queue cleared.';

	player.announceQueue(text, true);
	expect(player.queueAnnouncement).toEqual({ text, version: 1 });
	player.announceQueue(text, true);
	player.announceQueue('Order updated.');
	expect(player.queueAnnouncement).toEqual({ text: 'Order updated.', version: 2 });

	const library = player.queue.prepareLibrary([]);
	expect(library.next).toBe(initial);
	expect(player.commitQueue(library.next, 'replace', { sources: library.sources, isCurrent: () => true })).toBe(true);
	expect(player.queueAnnouncement).toEqual({ text: 'Order updated.', version: 2 });
	expect(player.replaceQueue(createQueue([]))).toBe(true);
	expect(player.queueAnnouncement).toEqual({ text: '', version: 2 });
	expect(other.queueAnnouncement).toEqual({ text: '', version: 0 });
});

test('an accepted replacement clears only its own browse-copy feedback', () => {
	const initial = createQueue([{ id: 'coral', title: 'Coral', artist: '', streamUrl: '/coral' }]);
	const effects = { flush: async () => {} };
	const player = createPlayerController(initial, effects);
	const other = createPlayerController(initial, effects);
	const empty = createQueue([]);
	for (const { browseCopy } of [player, other]) {
		browseCopy.settle(browseCopy.invalidate(), 'Copied link for Coral.', null);
	}

	try {
		expect(player.commitQueue(empty, 'replace', { sources: new Map(), isCurrent: () => false })).toBe(false);
		expect(player.browseCopy.feedback.status).toBe('Copied link for Coral.');
		expect(player.replaceQueue(empty)).toBe(true);
		expect(player.browseCopy.feedback).toEqual({ status: '', fallbackUrl: null });
		expect(other.browseCopy.feedback.status).toBe('Copied link for Coral.');
	} finally {
		clearMiniPlayerHint();
	}
});
