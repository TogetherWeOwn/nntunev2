import { describe, expect, it } from 'vitest';

import {
	ARTWORK_FALLBACK_ALT,
	ARTWORK_PLACEHOLDER_SRC,
	isUsableArtworkSrc,
	nextArtworkSrcAfterError,
	resetFailedArtworkSrcOnChange,
	resolveArtwork
} from './artwork.js';

describe('resolveArtwork', () => {
	it('passes a usable R2 url through with its alt text', () => {
		const resolved = resolveArtwork('https://r2.example.com/art/abc.jpg', {
			alt: 'Blue Train cover'
		});
		expect(resolved).toEqual({
			src: 'https://r2.example.com/art/abc.jpg',
			alt: 'Blue Train cover',
			isFallback: false
		});
	});

	it.each([null, undefined, '', '   '])('falls back for missing art %p', (src) => {
		const resolved = resolveArtwork(src, { alt: 'Kind of Blue' });
		expect(resolved.src).toBe(ARTWORK_PLACEHOLDER_SRC);
		expect(resolved.alt).toBe('Kind of Blue');
		expect(resolved.isFallback).toBe(true);
	});

	it('falls back for a non-url value and never leaves alt empty', () => {
		const resolved = resolveArtwork(42, { alt: '' });
		expect(resolved.src).toBe(ARTWORK_PLACEHOLDER_SRC);
		expect(resolved.alt).toBe(ARTWORK_FALLBACK_ALT);
		expect(resolved.isFallback).toBe(true);
	});

	it('never renders a broken-image state: src is always non-empty', () => {
		for (const src of [null, undefined, '', '   ', 0, {}, []]) {
			const resolved = resolveArtwork(src);
			expect(resolved.src.trim()).not.toBe('');
			expect(resolved.alt.trim()).not.toBe('');
		}
	});
});

describe('isUsableArtworkSrc', () => {
	it('rejects blanks and non-urls', () => {
		expect(isUsableArtworkSrc(null)).toBe(false);
		expect(isUsableArtworkSrc('   ')).toBe(false);
		expect(isUsableArtworkSrc('not a url')).toBe(false);
	});

	it('accepts path, data, blob and http(s) sources', () => {
		expect(isUsableArtworkSrc('/placeholder-artwork.svg')).toBe(true);
		expect(isUsableArtworkSrc('data:image/svg+xml;base64,AAA')).toBe(true);
		expect(isUsableArtworkSrc('blob:https://app.example/1')).toBe(true);
		expect(isUsableArtworkSrc('https://cdn.example/a.jpg')).toBe(true);
	});
});

describe('nextArtworkSrcAfterError', () => {
	it('swaps a failed R2 url to the placeholder once', () => {
		expect(nextArtworkSrcAfterError('https://r2.example.com/art/missing.jpg')).toBe(
			ARTWORK_PLACEHOLDER_SRC
		);
	});

	it('stops on the placeholder instead of looping', () => {
		expect(nextArtworkSrcAfterError(ARTWORK_PLACEHOLDER_SRC)).toBeNull();
	});
});

describe('resetFailedArtworkSrcOnChange', () => {
	it('clears a prior failure when the source changes away and back', () => {
		const failedSrc = 'https://r2.example.com/art/missing.jpg';
		const otherSrc = 'https://r2.example.com/art/other.jpg';

		const afterChange = resetFailedArtworkSrcOnChange(failedSrc, otherSrc, failedSrc);
		const afterReturn = resetFailedArtworkSrcOnChange(otherSrc, failedSrc, afterChange);

		expect(afterChange).toBeNull();
		expect(afterReturn).toBeNull();
	});
});
