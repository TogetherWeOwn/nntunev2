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
// raw-glob seam as the acceptance source pin. The acceptance pin in
// `import-validation.test.ts` is intentionally loose (it only requires
// `src={`) and that file is acceptance-owned, so the exact binding and the
// helper derivation are pinned here instead.
const routeSources = import.meta.glob('../../routes/**/*.svelte', {
	query: '?raw',
	import: 'default',
	eager: true
}) as Record<string, string>;

describe('library player wiring', () => {
	const source = routeSources['../../routes/library/+page.svelte'];

	it('derives the stream URL through resolveStreamUrl', () => {
		expect(source).toMatch(/resolveStreamUrl\(audioTrack\)/);
		expect(source).toMatch(/const audioTrack = \$derived\(currentAudioTrack\(\)\)/);
	});

	it('renders audio only when a stream URL exists', () => {
		expect(source).toMatch(/\{#if streamUrl\}/);
	});

	it('binds the audio src to exactly the derived stream URL', () => {
		expect(source).toMatch(/<audio[^>]*\ssrc=\{streamUrl\}/);
		// Any other binding expression must fail — not just a literal.
		expect(source).not.toMatch(/<audio[^>]*\ssrc=\{(?!streamUrl\})/);
		expect(source).not.toMatch(/<audio[^>]*\ssrc=""/);
	});
});
