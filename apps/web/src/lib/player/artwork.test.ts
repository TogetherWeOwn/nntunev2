import { describe, expect, it } from 'vitest';

// Failing-first acceptance tests for TOG-25177 (built on TOG-25185).
// The Builder implements `./artwork.js`; this file must NOT be edited on
// the build card — only the implementation file may change.
//
// Contract pinned here: a missing/blank R2 artwork URL resolves to a
// placeholder `src` with meaningful `alt` text — never an empty `src`
// (broken-image state). A valid URL passes through untouched.
import { ARTWORK_PLACEHOLDER, resolveArtwork } from './artwork.js';

describe('resolveArtwork', () => {
	it('falls back to the placeholder when the R2 URL is missing', () => {
		for (const missing of [null, undefined, ''] as const) {
			const resolved = resolveArtwork(missing);

			expect(resolved.src).toBe(ARTWORK_PLACEHOLDER);
			expect(resolved.src).not.toBe('');
		}
	});

	it('falls back to the placeholder for blank/whitespace URLs', () => {
		const resolved = resolveArtwork('   ');

		expect(resolved.src).toBe(ARTWORK_PLACEHOLDER);
		expect(resolved.src).not.toBe('');
	});

	it('passes a valid artwork URL through unchanged', () => {
		const url = 'https://artwork.example.com/cover-abc123.jpg';

		expect(resolveArtwork(url).src).toBe(url);
	});

	it('always provides non-empty alt text (fallback and passthrough)', () => {
		expect(resolveArtwork(null).alt.trim().length).toBeGreaterThan(0);
		expect(resolveArtwork('https://artwork.example.com/cover.jpg').alt.trim().length).toBeGreaterThan(
			0
		);
	});

	it('exposes a non-empty placeholder constant', () => {
		expect(ARTWORK_PLACEHOLDER.trim().length).toBeGreaterThan(0);
	});
});
