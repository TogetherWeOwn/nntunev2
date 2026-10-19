import { describe, expect, it } from 'vitest';
import { render } from 'svelte/server';
import PlayerQueue from './PlayerQueue.svelte';
import {
	addTrack,
	createQueue,
	nextTrack,
	previousTrack,
	selectTrack,
	type QueueState,
	type Track
} from './queue.js';

/**
 * Acceptance contract for the player queue hooks on the reusable player
 * component (queue-order un-fixme, player side): rendering a `QueueState`
 * from the existing queue store must expose the same hooks the demo player
 * and the live queue-order spec rely on — `queue-item` elements carrying
 * `data-track-id` in queue order, a `now-playing` element naming the
 * current track, and a `player-next` transport button. Interactive
 * play-through is pinned separately by the browser spec
 * (`apps/web/tests/player-queue.spec.ts`); this suite uses the repo's
 * vitest SSR-render pattern, driving state through the real queue store
 * transitions instead of clicks.
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

// Deliberately non-alphabetical id/title order, and ids that do not sort
// the same way as the list order, so a component that sorts (or renders a
// hard-coded fixture) instead of following the given track list fails.
const FIRST = track('track-mango', 'Mango Season');
const SECOND = track('track-apple', 'Apple Cart');
const THIRD = track('track-zebra', 'Zebra Crossing');
const TRACKS = [FIRST, SECOND, THIRD];

function renderQueue(state: QueueState): string {
	return render(PlayerQueue, { props: { state } }).body;
}

/** Track ids carried by `queue-item` hooks, in document order. */
function queueItemIds(body: string): (string | null)[] {
	const ids: (string | null)[] = [];
	for (const tag of body.matchAll(/<[a-zA-Z][^>]*data-testid="queue-item"[^>]*>/g)) {
		ids.push(tag[0].match(/data-track-id="([^"]*)"/)?.[1] ?? null);
	}
	return ids;
}

/** Text content of the `now-playing` hook, or null when the hook is absent. */
function nowPlayingText(body: string): string | null {
	const match = body.match(
		/<([a-zA-Z][\w-]*)\b[^>]*data-testid="now-playing"[^>]*>([\s\S]*?)<\/\1>/
	);
	if (!match) return null;
	return (match[2] ?? '').replace(/<[^>]*>/g, ' ').replace(/\s+/g, ' ').trim();
}

describe('PlayerQueue queue hooks', () => {
	it('renders queue-item hooks in track-list order carrying the track ids', () => {
		const body = renderQueue(createQueue(TRACKS));

		expect(queueItemIds(body)).toEqual(['track-mango', 'track-apple', 'track-zebra']);
	});

	it('fills the queue in insertion order as tracks are added', () => {
		let state = createQueue([]);
		state = addTrack(state, THIRD);
		state = addTrack(state, FIRST);

		expect(queueItemIds(renderQueue(state))).toEqual(['track-zebra', 'track-mango']);
	});

	it('renders no queue-item hooks for an empty queue', () => {
		expect(queueItemIds(renderQueue(createQueue([])))).toEqual([]);
	});
});

describe('PlayerQueue now-playing hook', () => {
	it('names the first track of a freshly filled queue', () => {
		expect(nowPlayingText(renderQueue(createQueue(TRACKS)))).toContain('Mango Season');
	});

	it('exposes a player-next button and surfaces the second track after next', () => {
		const initial = renderQueue(createQueue(TRACKS));
		expect(initial).toMatch(/<button\b[^>]*data-testid="player-next"/);

		const advanced = nowPlayingText(renderQueue(nextTrack(createQueue(TRACKS))));
		expect(advanced).toContain('Apple Cart');
		expect(advanced).not.toContain('Mango Season');
	});

	it('follows prev back to the first track', () => {
		const back = previousTrack(nextTrack(createQueue(TRACKS)));

		expect(nowPlayingText(renderQueue(back))).toContain('Mango Season');
	});

	it('follows selection to the selected track without reordering the queue', () => {
		const body = renderQueue(selectTrack(createQueue(TRACKS), 2));

		const selected = nowPlayingText(body);
		expect(selected).toContain('Zebra Crossing');
		expect(selected).not.toContain('Mango Season');
		expect(queueItemIds(body)).toEqual(['track-mango', 'track-apple', 'track-zebra']);
	});
});
