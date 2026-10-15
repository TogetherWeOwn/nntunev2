import { describe, expect, it, vi } from 'vitest';

import { SeekController, buildRangeHeader, type SeekFetch } from './seekController.js';

function okResponse(status = 206): Response {
	return new Response(new Uint8Array([1, 2, 3]), { status });
}

/** A fetch that hangs until its signal aborts, then rejects like fetch does. */
function hangingFetch(): SeekFetch {
	return (_url, { signal }) =>
		new Promise<Response>((_resolve, reject) => {
			signal.addEventListener('abort', () => {
				reject(new DOMException('The operation was aborted.', 'AbortError'));
			});
		});
}

describe('buildRangeHeader', () => {
	it('builds open and closed byte ranges', () => {
		expect(buildRangeHeader(0, 99)).toBe('bytes=0-99');
		expect(buildRangeHeader(45000)).toBe('bytes=45000-');
	});
});

describe('SeekController', () => {
	it('sends the Range header for a seek', async () => {
		const seen: Array<Record<string, string>> = [];
		const fetchFn: SeekFetch = async (_url, { headers }) => {
			seen.push(headers);
			return okResponse();
		};
		const spy = vi.fn(fetchFn);
		const controller = new SeekController('https://audio.example/track', spy);

		const result = await controller.seek(0, 99);

		expect(result.status).toBe(206);
		expect(spy).toHaveBeenCalledOnce();
		expect(seen[0]).toEqual({ range: 'bytes=0-99' });
		expect(controller.completed).toBe(1);
		expect(controller.pending).toBe(false);
	});

	it('aborts the in-flight seek when a new seek starts', async () => {
		const fetchFn = vi.fn(hangingFetch());
		const controller = new SeekController('https://audio.example/track', fetchFn);

		const first = controller.seek(0, 99);
		const second = controller.seek(200, 299);
		void second.catch(() => {});
		await expect(first).rejects.toMatchObject({ name: 'AbortError' });

		expect(controller.aborted).toBe(1);
		expect(controller.generation).toBe(2);
		controller.abort();
		await expect(second).rejects.toMatchObject({ name: 'AbortError' });
	});

	it('survives a 10x sequential seek storm without wedging', async () => {
		const fetchFn: SeekFetch = async (_url, { signal }) => {
			// First nine seeks are superseded before they resolve.
			await new Promise<void>((resolve, reject) => {
				const timer = setTimeout(resolve, 50);
				signal.addEventListener('abort', () => {
					clearTimeout(timer);
					reject(new DOMException('The operation was aborted.', 'AbortError'));
				});
			});
			return okResponse();
		};
		const spy = vi.fn(fetchFn);
		const controller = new SeekController('https://audio.example/track', spy);

		const attempts = Array.from({ length: 10 }, (_, i) =>
			controller.seek(i * 1000, i * 1000 + 99)
		);
		const outcomes = await Promise.allSettled(attempts);
		const fulfilled = outcomes.filter((o) => o.status === 'fulfilled');
		const rejected = outcomes.filter((o) => o.status === 'rejected');

		// Only the last seek survives; the nine predecessors abort.
		expect(fulfilled).toHaveLength(1);
		expect(rejected).toHaveLength(9);
		expect(controller.aborted).toBe(9);
		expect(controller.generation).toBe(10);
		expect(controller.pending).toBe(false);

		// The player still works after the storm: the next seek completes.
		const after = await controller.seek(0, 10);
		expect(after.status).toBe(206);
		expect(after.bytes.length).toBeGreaterThan(0);
		expect(controller.completed).toBe(2);
	});

	it('explicit abort leaves the controller reusable', async () => {
		const fetchFn = vi.fn(hangingFetch());
		const controller = new SeekController('https://audio.example/track', fetchFn);

		const pending = controller.seek(0, 99);
		controller.abort();
		await expect(pending).rejects.toMatchObject({ name: 'AbortError' });
		expect(controller.pending).toBe(false);

		const okFetch = vi.fn(async () => okResponse(200));
		const fresh = new SeekController('https://audio.example/track', okFetch);
		const result = await fresh.seek(0);
		expect(result.status).toBe(200);
	});
});
