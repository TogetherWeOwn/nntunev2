import { expect, test, vi } from 'vitest';
import { clearMiniPlayerHint, onMiniPlayerHint, type MiniPlayerHint } from '../shell/mini-player.js';
import { createPlayerController } from './controller.svelte.js';
import { createQueue } from './queue.js';

test('queue commits publish only display metadata and skip same-object Library refreshes', () => {
	const player = createPlayerController(createQueue([]), {
		flush: async () => {}, clearShareFeedback: () => {}, announce: () => {}, clearQueueAnnouncement: () => {}
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
