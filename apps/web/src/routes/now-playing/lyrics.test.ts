import { getAppPlayer } from '#lib/player/AppPlayer.svelte';
import { createPlayerController } from '#lib/player/controller.svelte.js';
import type { TrackLyrics } from '#lib/player/LyricsPanel.svelte';
import { createQueue, nextTrack, type Track } from '#lib/player/queue.js';
import { clearMiniPlayerHint } from '#lib/shell/mini-player.js';
import { render } from 'svelte/server';
import { afterEach, expect, it, vi } from 'vitest';
import NowPlaying from './+page.svelte';

vi.mock('$app/navigation', () => ({ afterNavigate: vi.fn() }));
vi.mock('$app/state', () => ({ page: { state: {} } }));
vi.mock('#lib/player/AppPlayer.svelte', () => ({ getAppPlayer: vi.fn() }));

// These tracks deliberately share display metadata: only the selected ID counts.
const tracks: Track[] = ['first', 'second'].map((id) => ({
	id,
	title: 'Same Title',
	artist: 'Fixture Band',
	streamUrl: `/fixture/${id}`
}));

afterEach(() => {
	vi.mocked(getAppPlayer).mockReset();
	clearMiniPlayerHint();
});

function draw() {
	const player = createPlayerController(createQueue(tracks), { flush: async () => {} });
	vi.mocked(getAppPlayer).mockReturnValue(player);
	return { player, body: (lyrics?: TrackLyrics) => render(NowPlaying, { props: { lyrics } }).body };
}

function supplied(trackId: string, ...texts: string[]): TrackLyrics {
	return {
		trackId,
		result: { source: 'embedded', cues: texts.map((text, index) => ({ atMs: index * 500, text })) }
	};
}

function expectNoLyrics(body: string) {
	expect(body).not.toMatch(/<(details|summary)\b/);
}

it('omits Lyrics without a matching non-empty result', () => {
	const { body } = draw();
	expect(body()).toContain('Same Title');
	expectNoLyrics(body());
	expectNoLyrics(body(supplied('first')));
	expectNoLyrics(body(supplied('first', '', ' ')));
	const mismatched = body(supplied('second', 'Not selected yet'));
	expectNoLyrics(mismatched);
	expect(mismatched).not.toContain('Not selected yet');
});

it('renders supplied lines in order as text in a closed native disclosure', () => {
	const { body } = draw();
	const html = body(supplied('first', 'Paper moon', '', '<em>Blue signal</em>', 'Last mirror'));
	expect(html).toMatch(/<summary\b[^>]*>Lyrics<\/summary>/);
	expect(html).not.toMatch(/<details\b[^>]*\sopen\b/);
	expect(Array.from(html.matchAll(/<li\b[^>]*>(.*?)<\/li>/g), ([, text]) => text)).toEqual([
		'Paper moon',
		expect.stringMatching(/^&lt;em(?:>|&gt;)Blue signal&lt;\/em(?:>|&gt;)$/),
		'Last mirror'
	]);
	expect(html).not.toContain('<em>');
});

it('drops stale text when the queue advances or clears and accepts the new selected result', () => {
	const { body, player } = draw();
	const first = supplied('first', 'Old line');
	const second = supplied('second', 'New line');
	expect(body(first)).toContain('Old line');

	expect(player.commitQueue(nextTrack(player.queue.snapshot), 'select')).toBe(true);
	const stale = body(first);
	expectNoLyrics(stale);
	expect(stale).not.toContain('Old line');
	expect(body(second)).toContain('New line');

	expect(player.commitQueue(createQueue([]), 'clear')).toBe(true);
	const empty = body(second);
	expectNoLyrics(empty);
	expect(empty).not.toContain('New line');
});
