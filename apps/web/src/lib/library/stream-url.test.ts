import { describe, expect, it } from 'vitest';

import { resolveStreamUrl } from './stream-url.js';

describe('resolveStreamUrl', () => {
	it('returns empty for a null or missing track', () => {
		expect(resolveStreamUrl(null)).toBe('');
		expect(resolveStreamUrl(undefined)).toBe('');
	});

	it('returns empty for a missing or non-string streamUrl', () => {
		expect(resolveStreamUrl({})).toBe('');
		expect(resolveStreamUrl({ streamUrl: undefined })).toBe('');
		expect(resolveStreamUrl({ streamUrl: null })).toBe('');
		expect(resolveStreamUrl({ streamUrl: 42 })).toBe('');
	});

	it('returns empty for an empty streamUrl', () => {
		expect(resolveStreamUrl({ streamUrl: '' })).toBe('');
	});

	it('returns empty for a whitespace-only streamUrl', () => {
		for (const blank of ['   ', '\t\n ', ' ']) {
			expect(resolveStreamUrl({ streamUrl: blank }), JSON.stringify(blank)).toBe('');
		}
	});

	it('returns a real stream URL unchanged', () => {
		expect(resolveStreamUrl({ streamUrl: '/api/demo/audio?track=x' })).toBe(
			'/api/demo/audio?track=x'
		);
	});

	it('trims padding around a real stream URL', () => {
		expect(resolveStreamUrl({ streamUrl: '  /api/demo/audio  ' })).toBe('/api/demo/audio');
	});
});

// Engineering Lead authorized one-time same-PR physical player/queue owner
// source-wiring migration only. Behavior vectors remain protected and require
// independent equivalence review.
// Follow each production connection separately; unrelated source tokens cannot
// substitute for normalization reaching the selected layout-owned audio.
const sources = import.meta.glob([
	'../../routes/library/+page.svelte', '../../routes/+layout.svelte',
	'../player/PlayerAudio.svelte', '../player/context.ts', '../player/session.ts'
], {
	query: '?raw',
	import: 'default',
	eager: true
}) as Record<string, string>;

describe('library player wiring', () => {
	const library = sources['../../routes/library/+page.svelte'];
	const audio = sources['../player/PlayerAudio.svelte'];

	it('normalizes a selected library item before it enters the shared queue', () => {
		expect(library).toMatch(/function playableTrackFor\(item: LibraryItem\): Track \{\s*return \{ \.\.\.queuedTrackFor\(item\), streamUrl: resolveStreamUrl\(\{ streamUrl: item\.streamUrl \}\) \};\s*\}/);
		expect(library).toContain('replaceQueue(addTrack(queue, playableTrackFor(item)))');
		expect(library).toContain('player.setQueue(next)');
		expect(library).toContain('const player = usePlayerSession()');
	});

	it('shares the selected queue through the layout-provided session', () => {
		const layout = sources['../../routes/+layout.svelte'];
		expect(layout).toContain('const player = createPlayerSession({ flush: tick })');
		expect(layout).toContain('providePlayerSession(player)');
		// Authorized same-PR owner-wiring pin: presentation props keep this decoder mount.
		expect(layout).toContain('<PlayerAudio {queueToggle} expanded={nowPlayingRoute} viewHeader={nowPlayingHeader} />');
		const context = sources['../player/context.ts'];
		expect(context).toContain('setContext(PLAYER_SESSION, session)');
		expect(context).toContain('getContext<PlayerSession>(PLAYER_SESSION)');
		const session = sources['../player/session.ts'];
		expect(session).toContain("import { writable } from 'svelte/store'");
		expect(session).toContain('publish({ queue,');
		expect(session).toContain('subscribe: store.subscribe');
		expect(audio).toContain('const player = usePlayerSession()');
		expect(audio).toContain('const selected = $derived(currentTrack($player.queue))');
	});

	it('renders audio only when a normalized stream URL exists', () => {
		expect(audio).toContain('{#if selected?.streamUrl}');
	});

	it('binds the audio src to exactly the selected normalized stream URL', () => {
		expect(audio).toMatch(/<audio[^>]*\ssrc=\{selected\.streamUrl\}/);
		// Any other binding expression must fail — not just a literal.
		expect(audio).not.toMatch(/<audio[^>]*\ssrc=\{(?!selected\.streamUrl\})/);
		expect(audio).not.toMatch(/<audio[^>]*\ssrc=""/);
	});
});
