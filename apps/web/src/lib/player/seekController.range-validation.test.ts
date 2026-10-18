import { describe, expect, it, vi } from 'vitest';

import { SeekController, buildRangeHeader } from './seekController.js';

describe('buildRangeHeader range validation', () => {
	it.each([NaN, Infinity, -Infinity])('throws RangeError for start=%s', (start) => {
		expect(() => buildRangeHeader(start)).toThrow(RangeError);
	});

	it.each([NaN, Infinity, -Infinity])('throws RangeError for end=%s', (end) => {
		expect(() => buildRangeHeader(0, end)).toThrow(RangeError);
	});

	it.each([NaN, Infinity, -Infinity])('seek(%s) rejects before any fetch', async (start) => {
		const fetchFn = vi.fn(async () => new Response(new Uint8Array([1]), { status: 200 }));
		const controller = new SeekController('https://audio.example/track', fetchFn);

		await expect(controller.seek(start)).rejects.toThrow(RangeError);
		expect(fetchFn).not.toHaveBeenCalled();
	});

	it.each([NaN, Infinity, -Infinity])('seek(0, %s) rejects before any fetch', async (end) => {
		const fetchFn = vi.fn(async () => new Response(new Uint8Array([1]), { status: 200 }));
		const controller = new SeekController('https://audio.example/track', fetchFn);

		await expect(controller.seek(0, end)).rejects.toThrow(RangeError);
		expect(fetchFn).not.toHaveBeenCalled();
	});

	it('ordinary seeks produce byte-identical headers', () => {
		expect(buildRangeHeader(0, 99)).toBe('bytes=0-99');
		expect(buildRangeHeader(45000)).toBe('bytes=45000-');
	});
});
