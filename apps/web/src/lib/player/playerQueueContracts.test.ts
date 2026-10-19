import { describe, expect, it } from 'vitest';
import { render } from 'svelte/server';
import PlayerQueue from './PlayerQueue.svelte';
import { createQueue, selectTrack, type QueueState, type Track } from './queue.js';

/**
 * Builder-owned regression pins for PlayerQueue contracts the acceptance
 * suite (playerQueue.test.ts, Test-Author owned — do not merge into it)
 * exercises only implicitly: the empty-queue contract (empty now-playing
 * text, Next disabled only when empty) and the aria-current marking of the
 * playing item.
 */

function track(id: string, title: string): Track {
	return {
		id,
		title,
		artist: 'Contract Band',
		streamUrl: `/api/demo/audio?track=${id}`,
		artworkUrl: null
	};
}

const FIRST = track('track-mango', 'Mango Season');
const SECOND = track('track-apple', 'Apple Cart');
const THIRD = track('track-zebra', 'Zebra Crossing');

function renderQueue(state: QueueState): string {
	return render(PlayerQueue, { props: { state } }).body;
}

/** Opening tags of the `queue-item` hooks, in document order. */
function queueItemTags(body: string): string[] {
	return [...body.matchAll(/<[a-zA-Z][^>]*data-testid="queue-item"[^>]*>/g)].map((m) => m[0]);
}

/** Text content of the `now-playing` hook, or null when the hook is absent. */
function nowPlayingText(body: string): string | null {
	const match = body.match(
		/<([a-zA-Z][\w-]*)\b[^>]*data-testid="now-playing"[^>]*>([\s\S]*?)<\/\1>/
	);
	if (!match) return null;
	return (match[2] ?? '').replace(/<[^>]*>/g, ' ').replace(/\s+/g, ' ').trim();
}

/** Opening tag of the `player-next` button, or null when absent. */
function playerNextTag(body: string): string | null {
	return body.match(/<[a-zA-Z][^>]*data-testid="player-next"[^>]*>/)?.[0] ?? null;
}

/** Text content of each `queue-item` hook, in document order. */
function queueItemTexts(body: string): string[] {
	return [
		...body.matchAll(/<([a-zA-Z][\w-]*)\b[^>]*data-testid="queue-item"[^>]*>([\s\S]*?)<\/\1>/g)
	].map((m) => (m[2] ?? '').replace(/<[^>]*>/g, ' ').replace(/\s+/g, ' ').trim());
}

describe('PlayerQueue empty-queue contract', () => {
	it('names the empty state and disables Next when the queue is empty', () => {
		const body = renderQueue(createQueue([]));

		expect(nowPlayingText(body)).toBe('Queue is empty');
		expect(playerNextTag(body)).toMatch(/disabled/);
	});

	it('enables Next when the queue is non-empty', () => {
		const body = renderQueue(createQueue([FIRST, SECOND]));

		expect(playerNextTag(body)).not.toMatch(/disabled/);
	});
});

describe('PlayerQueue current-item marking', () => {
	it('marks exactly the first item current on a fresh queue', () => {
		const tags = queueItemTags(renderQueue(createQueue([FIRST, SECOND, THIRD])));

		expect(tags).toHaveLength(3);
		expect(tags[0]).toContain('aria-current="true"');
		expect(tags[1]).not.toContain('aria-current');
		expect(tags[2]).not.toContain('aria-current');
	});

	it('moves the marking with selection without reordering', () => {
		const body = renderQueue(selectTrack(createQueue([FIRST, SECOND, THIRD]), 2));
		const tags = queueItemTags(body);

		expect(tags).toHaveLength(3);
		expect(tags[0]).not.toContain('aria-current');
		expect(tags[1]).not.toContain('aria-current');
		expect(tags[2]).toContain('aria-current="true"');
		expect(tags.map((t) => t.match(/data-track-id="([^"]*)"/)?.[1])).toEqual([
			'track-mango',
			'track-apple',
			'track-zebra'
		]);
	});
});

describe('PlayerQueue artist-less tracks', () => {
	it('omits the dangling separator when the track has no artist', () => {
		// Library rows without an artist reach the panel as an empty artist
		// string; the separator must go with the missing artist, not dangle.
		const noArtist: Track = { ...FIRST, id: 'track-solo', title: 'Solo Song', artist: '' };
		const body = renderQueue(createQueue([noArtist]));

		expect(nowPlayingText(body)).toBe('Now playing: Solo Song');
		expect(queueItemTexts(body)).toEqual(['Solo Song']);
	});
});
