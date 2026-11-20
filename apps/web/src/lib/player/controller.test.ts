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
		expect(player.reconcileLibrary(next.tracks, () => true)).toBe(true);
		const previous = player.queue.sourceForQueuedPlay('selected');
		expect(previous?.streamUrl).toBe('/test-audio/selected');
		const versions = () => [player.mediaRevision, player.media.attachmentVersion, player.queue.revision];
		const before = versions();
		expect(player.reconcileLibrary([
			next.tracks[0],
			{ id: 'selected', streamUrl: '/test-audio/z-first' },
			{ id: 'selected', streamUrl: '/test-audio/a-second' }
		], () => true)).toBe(true);
		expect(player.queue.snapshot).toBe(next);
		expect(versions()).toEqual(before);
		expect(previous?.isCurrent()).toBe(false);
		expect(player.queue.hasLibrarySource('selected', '/test-audio/selected')).toBe(false);
		expect(player.queue.sourceForQueuedPlay('selected')?.streamUrl).toBe('/test-audio/z-first');
		expect(player.queue.sourceForQueuedPlay('selected', '/test-audio/a-second')?.streamUrl).toBe('/test-audio/a-second');
		expect(player.queue.sourceForQueuedPlay('selected', '/test-audio/selected')?.streamUrl).toBe('/test-audio/selected');
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

test('an authoritative empty snapshot clears queue and all Library evidence', () => {
	const initial = createQueue([{ id: 'coral', title: 'Coral', artist: '', streamUrl: '/coral' }]);
	const player = createPlayerController(initial, { flush: async () => {} });
	expect(player.reconcileLibrary([...initial.tracks, { id: 'reef', streamUrl: '/reef' }], () => true)).toBe(true);
	const previous = player.queue.sourceForQueuedPlay('reef');
	expect(previous?.streamUrl).toBe('/reef');

	try {
		expect(player.reconcileLibrary([], () => true)).toBe(true);
		expect(player.queue.snapshot).toEqual(createQueue([]));
		expect(player.queue.sourceForQueuedPlay('coral')).toBeNull();
		expect(player.queue.sourceForQueuedPlay('reef')).toBeNull();
		expect(previous?.isCurrent()).toBe(false);
	} finally {
		clearMiniPlayerHint();
	}
});

test.each([false, true])('a receipt superseded in the before-commit hook cannot publish (queue changed: %s)', (queueChanged) => {
	const initial = createQueue([{ id: 'coral', title: 'Coral', artist: '', streamUrl: '/coral' }]);
	const player = createPlayerController(initial, { flush: async () => {} });
	expect(player.reconcileLibrary(initial.tracks, () => true)).toBe(true);
	let current = true;
	const staleItems = queueChanged ? [] : [{ id: 'coral', streamUrl: '/stale' }];
	expect(player.reconcileLibrary(staleItems, () => current, (changed) => {
		expect(changed).toBe(queueChanged);
		expect(player.queue.snapshot).toBe(initial);
		expect(player.queue.sourceForQueuedPlay('coral')?.streamUrl).toBe('/coral');
		current = false;
		expect(player.reconcileLibrary([{ id: 'coral', streamUrl: '/fresh' }], () => true)).toBe(true);
	})).toBe(false);
	expect(player.queue.snapshot).toBe(initial);
	expect(player.queue.sourceForQueuedPlay('coral')?.streamUrl).toBe('/fresh');
	expect(player.queue.hasLibrarySource('coral', '/stale')).toBe(false);
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

	expect(player.reconcileLibrary([], () => true)).toBe(true);
	expect(player.queue.snapshot).toBe(initial);
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
		expect(player.reconcileLibrary(initial.tracks, () => true)).toBe(true);
		const previous = player.queue.sourceForQueuedPlay('coral');
		const beforeCommit = vi.fn();
		expect(player.reconcileLibrary([], () => false, beforeCommit)).toBe(false);
		expect(beforeCommit).not.toHaveBeenCalled();
		expect(player.queue.snapshot).toBe(initial);
		expect(previous?.isCurrent()).toBe(true);
		expect(player.browseCopy.feedback.status).toBe('Copied link for Coral.');
		expect(player.replaceQueue(empty)).toBe(true);
		expect(player.browseCopy.feedback).toEqual({ status: '', fallbackUrl: null });
		expect(other.browseCopy.feedback.status).toBe('Copied link for Coral.');
	} finally {
		clearMiniPlayerHint();
	}
});

test.each([
	['success', { status: 'Copied link for Coral.', fallbackUrl: null }],
	['manual fallback', { status: '', fallbackUrl: 'https://example.test/library?track=coral' }]
] as const)('auth reset retires a browse-copy receipt for %s on that player only', (_, outcome) => {
	const initial = createQueue([]);
	const effects = { flush: async () => {} };
	const player = createPlayerController(initial, effects);
	const other = createPlayerController(initial, effects);
	const old = player.browseCopy.invalidate();
	const otherReceipt = other.browseCopy.invalidate();
	const empty = { status: '', fallbackUrl: null };
	player.browseCopy.settle(old, outcome.status, outcome.fallbackUrl);
	other.browseCopy.settle(otherReceipt, 'Copied another link.', null);

	player.resetAtAuthBoundary();
	expect(player.browseCopy.feedback).toEqual(empty);
	expect(other.browseCopy.feedback).toEqual({ status: 'Copied another link.', fallbackUrl: null });
	player.browseCopy.settle(old, outcome.status, outcome.fallbackUrl);
	expect(player.browseCopy.feedback).toEqual(empty);
	other.browseCopy.settle(otherReceipt, outcome.status, outcome.fallbackUrl);
	expect(other.browseCopy.feedback).toEqual(outcome);
	expect(player.browseCopy.feedback).toEqual(empty);

	const fresh = player.browseCopy.invalidate();
	player.browseCopy.settle(fresh, outcome.status, outcome.fallbackUrl);
	expect(player.browseCopy.feedback).toEqual(outcome);
});
