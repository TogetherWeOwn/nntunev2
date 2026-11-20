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

// Builder-owned exact pins for the library player wiring, via the same Vite
// raw-glob seam as the other route-markup pins. The exact binding and the
// helper derivation are pinned here.
const sources = import.meta.glob(['../../routes/+layout.svelte', '../../routes/library/+page.svelte', '../player/AppPlayer.svelte', '../player/NativeAudio.svelte', '../player/NativePlayer.svelte'], {
	query: '?raw',
	import: 'default',
	eager: true
}) as Record<string, string>;

describe('library player wiring', () => {
	const source = sources['../player/AppPlayer.svelte'];
	const player = sources['../player/NativePlayer.svelte'];
	const host = sources['../player/NativeAudio.svelte'];

	it('derives the stream URL through resolveStreamUrl', () => {
		expect(source).toMatch(/resolveStreamUrl\(currentTrack\)/);
	});

	it('renders audio only when a stream URL exists', () => {
		expect(source).toMatch(/\{#if streamUrl\}\s*<div class="nt-wrap">\s*<NativePlayer\b[^>]*\/>\s*<\/div>\s*\{\/if\}/);
		expect(player).toMatch(/<section aria-label="Player">/);
		expect(sources['../../routes/library/+page.svelte']).not.toContain('<NativePlayer');
		expect(sources['../../routes/+layout.svelte']).toMatch(/\{#if page\.route\.id === null\}\s*\{@render appBody\(\)\}\s*\{:else\}\s*<AppPlayer\b/);
		expect(source + player).not.toMatch(/<audio\b/);
	});

	it('binds the audio src to exactly the derived stream URL', () => {
		expect(source).toMatch(/<NativePlayer\b[^>]*\s\{streamUrl\}/);
		expect(player).toMatch(/<NativeAudio\b[^>]*\s\{streamUrl\}/);
		expect(host).toMatch(/<audio[^>]*\ssrc=\{streamUrl\}/);
		// Any other binding expression must fail — not just a literal.
		expect(host).not.toMatch(/<audio[^>]*\ssrc=\{(?!streamUrl\})/);
		expect(host).not.toMatch(/<audio[^>]*\ssrc=""/);
	});
});
