import { describe, expect, it } from 'vitest';
import { render } from 'svelte/server';
import PlayerQueue from './PlayerQueue.svelte';
import {
	addTrack,
	createQueue,
	nextTrack,
	type QueueState,
	type Track
} from './queue.js';

/**
 * Acceptance: player queue announcements and accessible names.
 *
 * - The `now-playing` hook must be a polite live region
 *   (`role="status"` or `aria-live="polite"`, never assertive), so a
 *   track change is announced without interrupting the listener.
 * - That region must stay silent on unrelated re-renders: its content
 *   changes only when the current track changes — appending a
 *   non-current track, or re-rendering the same state, leaves the
 *   announcement text byte-identical.
 * - The queue list itself must carry an accessible name
 *   (`aria-label`, or `aria-labelledby` pointing at rendered text).
 *
 * Existing hooks (`now-playing`, `queue-item`, `player-next`) and queue
 * behavior are unchanged; `playerQueue.test.ts` pins those. The Builder
 * must not edit this file.
 */

function track(id: string, title: string): Track {
	return {
		id,
		title,
		artist: 'Acceptance Band',
		streamUrl: `/api/demo/audio?track=${id}`,
		artworkUrl: null
	};
}

const FIRST = track('track-mango', 'Mango Season');
const SECOND = track('track-apple', 'Apple Cart');
const THIRD = track('track-zebra', 'Zebra Crossing');
const EXTRA = track('track-kiwi', 'Kiwi Stand');

function renderQueue(state: QueueState): string {
	return render(PlayerQueue, { props: { state } }).body;
}

/** The full `now-playing` element markup, or null when the hook is absent. */
function nowPlayingElement(body: string): string | null {
	return (
		body.match(/<([a-zA-Z][\w-]*)\b[^>]*data-testid="now-playing"[^>]*>[\s\S]*?<\/\1>/)?.[0] ??
		null
	);
}

/** Opening tag of the `now-playing` element, or null when absent. */
function nowPlayingTag(body: string): string | null {
	return body.match(/<[a-zA-Z][\w-]*\b[^>]*data-testid="now-playing"[^>]*>/)?.[0] ?? null;
}

function nowPlayingText(body: string): string | null {
	const element = nowPlayingElement(body);
	if (!element) return null;
	return element
		.replace(/<[^>]*>/g, ' ')
		.replace(/\s+/g, ' ')
		.trim();
}

/** Opening tag of the list that contains the queue items, or null. */
function queueListTag(body: string): string | null {
	return body.match(/<(ol|ul)\b[^>]*>/)?.[0] ?? null;
}

describe('PlayerQueue now-playing live region', () => {
	it('announces now-playing politely, never assertively', () => {
		const tag = nowPlayingTag(renderQueue(createQueue([FIRST, SECOND])));

		expect(tag).not.toBeNull();
		const polite = /role="status"/.test(tag ?? '') || /aria-live="polite"/.test(tag ?? '');
		expect(polite).toBe(true);
		expect(tag).not.toMatch(/aria-live="assertive"/);
		expect(tag).not.toMatch(/role="alert"/);
	});

	it('keeps the same polite region when the queue is empty', () => {
		const body = renderQueue(createQueue([]));
		const tag = nowPlayingTag(body);

		expect(tag).not.toBeNull();
		const polite = /role="status"/.test(tag ?? '') || /aria-live="polite"/.test(tag ?? '');
		expect(polite).toBe(true);
		expect(nowPlayingText(body)).toContain('Queue is empty');
	});

	it('does not change the announcement when a non-current track is appended', () => {
		const before = createQueue([FIRST, SECOND]);
		const after = addTrack(before, EXTRA);

		expect(nowPlayingText(renderQueue(after))).toBe(nowPlayingText(renderQueue(before)));
		expect(nowPlayingText(renderQueue(after))).toContain('Mango Season');
	});

	it('renders byte-identical now-playing markup for an unchanged state', () => {
		const state = createQueue([FIRST, SECOND, THIRD]);

		expect(nowPlayingElement(renderQueue(state))).toBe(nowPlayingElement(renderQueue(state)));
	});

	it('changes the announcement when the current track advances', () => {
		const before = createQueue([FIRST, SECOND]);
		const after = nextTrack(before);

		expect(nowPlayingText(renderQueue(before))).toContain('Mango Season');
		expect(nowPlayingText(renderQueue(after))).toContain('Apple Cart');
		expect(nowPlayingText(renderQueue(after))).not.toBe(nowPlayingText(renderQueue(before)));
	});
});

describe('PlayerQueue queue list accessible name', () => {
	it('labels the queue list for assistive tech', () => {
		const body = renderQueue(createQueue([FIRST, SECOND]));
		const tag = queueListTag(body);

		expect(tag).not.toBeNull();
		const ariaLabel = tag?.match(/aria-label="([^"]+)"/)?.[1]?.trim();
		const labelledBy = tag?.match(/aria-labelledby="([^"]+)"/)?.[1]?.trim();
		const labelledByTarget =
			labelledBy !== undefined && labelledBy.length > 0
				? labelledBy.split(/\s+/).every((id) => body.includes(`id="${id}"`))
				: false;

		expect(
			(ariaLabel !== undefined && ariaLabel.length > 0) || labelledByTarget,
			`queue list has no accessible name: ${tag}`
		).toBe(true);
	});
});
