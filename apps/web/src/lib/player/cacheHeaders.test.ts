import { describe, expect, it } from 'vitest';

import {
	ARTWORK_CACHE_CONTROL,
	AUDIO_CACHE_CONTROL,
	artworkCacheHeaders,
	audioCacheHeaders,
	cacheControlFor
} from './cacheHeaders.js';

describe('audio cache headers', () => {
	it('is private with a short max-age', () => {
		expect(AUDIO_CACHE_CONTROL).toMatch(/^private/);
		expect(AUDIO_CACHE_CONTROL).toContain('max-age=');
		expect(AUDIO_CACHE_CONTROL).not.toContain('immutable');
	});

	it('is served on the helper', () => {
		expect(audioCacheHeaders()['cache-control']).toBe(AUDIO_CACHE_CONTROL);
		expect(cacheControlFor('audio')).toBe(AUDIO_CACHE_CONTROL);
	});
});

describe('artwork cache headers', () => {
	it('is public and immutable with a long max-age', () => {
		expect(ARTWORK_CACHE_CONTROL).toMatch(/^public/);
		expect(ARTWORK_CACHE_CONTROL).toContain('immutable');
		expect(ARTWORK_CACHE_CONTROL).toContain('max-age=31536000');
	});

	it('is served on the helper', () => {
		expect(artworkCacheHeaders()['cache-control']).toBe(ARTWORK_CACHE_CONTROL);
		expect(cacheControlFor('artwork')).toBe(ARTWORK_CACHE_CONTROL);
	});

	it('differs from audio so shared caches never store per-user bytes', () => {
		expect(ARTWORK_CACHE_CONTROL).not.toBe(AUDIO_CACHE_CONTROL);
	});
});
