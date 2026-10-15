import { describe, expect, it } from 'vitest';

// Failing-first acceptance tests for TOG-25177 (built on TOG-25185).
// The Builder implements `./cacheHeaders.js`; this file must NOT be edited
// on the build card — only the implementation file may change.
//
// Contract pinned here:
// - audio responses: `private` + `max-age`, never `public`/`immutable`
//   (per-user streams must not sit in shared caches).
// - artwork responses: `public` + `immutable` + long `max-age` (content
//   addressed, safe to cache aggressively).
import { artworkCacheHeader, audioCacheHeader, cacheHeaderFor } from './cacheHeaders.js';

describe('audioCacheHeader', () => {
	it('marks audio as private with a short max-age', () => {
		const header = audioCacheHeader();

		expect(header).toContain('private');
		expect(header).toMatch(/max-age=\d+/);
	});

	it('never marks audio as publicly cacheable or immutable', () => {
		const header = audioCacheHeader();

		expect(header).not.toContain('public');
		expect(header).not.toContain('immutable');
	});
});

describe('artworkCacheHeader', () => {
	it('marks artwork as public and immutable', () => {
		const header = artworkCacheHeader();

		expect(header).toContain('public');
		expect(header).toContain('immutable');
	});

	it('gives artwork a long max-age (at least a year)', () => {
		const header = artworkCacheHeader();
		const maxAge = Number(header.match(/max-age=(\d+)/)?.[1]);

		expect(Number.isFinite(maxAge)).toBe(true);
		expect(maxAge).toBeGreaterThanOrEqual(31_536_000);
	});
});

describe('cacheHeaderFor', () => {
	it('dispatches audio vs artwork to the matching helper', () => {
		expect(cacheHeaderFor('audio')).toBe(audioCacheHeader());
		expect(cacheHeaderFor('artwork')).toBe(artworkCacheHeader());
	});
});
