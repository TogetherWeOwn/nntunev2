import { describe, expect, it } from 'vitest';
import { render } from 'svelte/server';
import PlayerQueue from './PlayerQueue.svelte';
import { createQueue, type Track } from './queue.js';

// Builder-owned regression: complements the protected bulk-queue bank without
// editing it. Removing the explicit separator space must fail the rendered row.
describe('queue row title/artist separator', () => {
	it.each([
		{ title: 'Middle Song', artist: 'Alpha Band', expected: 'Middle Song — Alpha Band' },
		{ title: '  ', artist: 'Alpha Band', expected: 'untitled — Alpha Band' },
		{ title: 'Middle Song', artist: '', expected: 'Middle Song' }
	])('renders "$expected" consistently with now-playing', ({ title, artist, expected }) => {
		const track: Track = { id: 'untitled', title, artist, streamUrl: '' };
		const html = render(PlayerQueue, { props: { state: createQueue([track]) } }).body;
		const row = html.match(/<li\b[^>]*data-testid="queue-item"[^>]*>([\s\S]*?)<button\b/);
		expect(row, 'the rendered queue row is missing').not.toBeNull();
		// Comments delimit Svelte expressions but add no visible whitespace.
		const rowText = row![1].replace(/<!--[^]*?-->/g, '').replace(/\s+/g, ' ').trim();
		expect(rowText).toBe(expected);
		const nowPlaying = html.match(/<p\b[^>]*data-testid="now-playing"[^>]*>([\s\S]*?)<\/p>/);
		expect(nowPlaying, 'the rendered now-playing line is missing').not.toBeNull();
		const currentText = nowPlaying![1].replace(/<!--[^]*?-->/g, '').replace(/\s+/g, ' ').trim();
		expect(currentText).toBe(`Now playing: ${expected}`);
	});
});
