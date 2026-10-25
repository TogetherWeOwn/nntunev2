import { describe, expect, it } from 'vitest';
import { render } from 'svelte/server';
import { parse } from 'svelte/compiler';
import PlayerQueue from './PlayerQueue.svelte';
import { createQueue, type Track } from './queue.js';

// These SSR fragments contain only text and comments before the row controls.
// Parse text nodes so Svelte markers add no whitespace and entities decode.
function renderedText(markup: string): string {
	return parse(markup, { modern: true }).fragment.nodes
		.filter((node) => node.type === 'Text')
		.map((node) => node.data)
		.join('')
		.replace(/\s+/g, ' ')
		.trim();
}

// Builder-owned regression: complements the protected bulk-queue bank without
// editing it. Removing the explicit separator space must fail the rendered row.
describe('queue row title/artist separator', () => {
	it.each([
		{ title: 'Middle Song', artist: 'Alpha Band', expected: 'Middle Song — Alpha Band' },
		{ title: '  ', artist: 'Alpha Band', expected: 'untitled — Alpha Band' },
		{ title: 'Middle Song', artist: '', expected: 'Middle Song' },
		{ title: 'Rock & <Roll>', artist: 'Alpha Band', expected: 'Rock & <Roll> — Alpha Band' }
	])('renders "$expected" consistently with now-playing', ({ title, artist, expected }) => {
		const track: Track = { id: 'untitled', title, artist, streamUrl: '' };
		const html = render(PlayerQueue, { props: { state: createQueue([track]) } }).body;
		const row = html.match(/<li\b[^>]*data-testid="queue-item"[^>]*>([\s\S]*?)<button\b/);
		expect(row, 'the rendered queue row is missing').not.toBeNull();
		const rowText = renderedText(row![1]);
		expect(rowText).toBe(expected);
		const nowPlaying = html.match(/<p\b[^>]*data-testid="now-playing"[^>]*>([\s\S]*?)<\/p>/);
		expect(nowPlaying, 'the rendered now-playing line is missing').not.toBeNull();
		const currentText = renderedText(nowPlaying![1]);
		expect(currentText).toBe(`Now playing: ${expected}`);
	});
});
