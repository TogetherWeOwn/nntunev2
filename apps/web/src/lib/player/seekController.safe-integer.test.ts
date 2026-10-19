import { describe, expect, it, vi } from 'vitest';

import { SeekController, buildRangeHeader, type SeekFetch } from './seekController.js';

const UNSAFE = Number.MAX_SAFE_INTEGER + 1;

/** A fetch that hangs until its signal aborts, then rejects like fetch does. */
function hangingFetch(): SeekFetch {
	return (_url, { signal }) =>
		new Promise<Response>((_resolve, reject) => {
			signal.addEventListener('abort', () => {
				reject(new DOMException('The operation was aborted.', 'AbortError'));
			});
		});
}

describe('buildRangeHeader safe-integer validation', () => {
	it('throws RangeError for a finite non-safe-integer start', () => {
		expect(() => buildRangeHeader(UNSAFE)).toThrow(RangeError);
	});

	it('throws RangeError for a finite non-safe-integer end', () => {
		expect(() => buildRangeHeader(0, UNSAFE)).toThrow(RangeError);
	});
});

describe('SeekController safe-integer rejection', () => {
	it('seek(start) with 2^53 rejects with RangeError and performs zero fetches', async () => {
		const fetchFn = vi.fn(async () => new Response(new Uint8Array([1]), { status: 200 }));
		const controller = new SeekController('https://audio.example/track', fetchFn);

		await expect(controller.seek(UNSAFE)).rejects.toThrow(RangeError);
		expect(fetchFn).not.toHaveBeenCalled();
	});

	it('seek(0, end) with 2^53 rejects with RangeError and performs zero fetches', async () => {
		const fetchFn = vi.fn(async () => new Response(new Uint8Array([1]), { status: 200 }));
		const controller = new SeekController('https://audio.example/track', fetchFn);

		await expect(controller.seek(0, UNSAFE)).rejects.toThrow(RangeError);
		expect(fetchFn).not.toHaveBeenCalled();
	});

	it('a rejected unsafe-integer call does not abort an in-flight seek', async () => {
		const fetchFn = vi.fn(hangingFetch());
		const controller = new SeekController('https://audio.example/track', fetchFn);

		const first = controller.seek(0, 99);
		void first.catch(() => {});
		await expect(controller.seek(UNSAFE)).rejects.toThrow(RangeError);

		expect(fetchFn).toHaveBeenCalledTimes(1);
		expect(controller.aborted).toBe(0);
		expect(controller.generation).toBe(1);
		expect(controller.pending).toBe(true);

		controller.abort();
		await expect(first).rejects.toMatchObject({ name: 'AbortError' });
	});
});

describe('buildRangeHeader in-range behavior is unchanged', () => {
	it('fractional in-range values still use Math.floor and do not throw', () => {
		expect(buildRangeHeader(1.5)).toBe('bytes=1-');
		expect(buildRangeHeader(1.5, 3.9)).toBe('bytes=1-3');
	});

	it.each([NaN, Infinity, -Infinity])('non-finite start=%s still throws RangeError', (start) => {
		expect(() => buildRangeHeader(start)).toThrow(RangeError);
	});

	it.each([NaN, Infinity, -Infinity])('non-finite end=%s still throws RangeError', (end) => {
		expect(() => buildRangeHeader(0, end)).toThrow(RangeError);
	});
});
