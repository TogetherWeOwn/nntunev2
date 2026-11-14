import { describe, expect, it, vi } from 'vitest';

import { SeekController } from './seekController.js';

const policies = [
	{ name: 'finite 200 prefix', status: 200, end: 1, bounded: true, exact: false },
	{ name: 'finite 206 exact', status: 206, end: 1, bounded: true, exact: true },
	{ name: 'open-ended 206', status: 206, end: undefined, bounded: false, exact: true },
	{ name: 'full 200', status: 200, end: undefined, bounded: false, exact: false }
];

function responseBody(chunks: Uint8Array[]) {
	let pulls = 0;
	const cancel = vi.fn();
	const stream = new ReadableStream<Uint8Array>(
		{
			pull(controller) {
				pulls += 1;
				const chunk = chunks.shift();
				if (chunk === undefined) controller.close();
				else controller.enqueue(chunk);
			},
			cancel
		},
		{ highWaterMark: 0 }
	);
	return { stream, cancel, pulls: () => pulls };
}

describe('SeekController shared reader policies', () => {
	for (const policy of policies) {
		for (const trailingBytes of [false, true]) {
			it(`${policy.name} preserves consumption and cleanup with trailing bytes=${trailingBytes}`, async () => {
				const chunks = [new Uint8Array([1, 2]), new Uint8Array(), new Uint8Array()];
				if (trailingBytes) chunks.push(new Uint8Array([3, 4]));
				const body = responseBody(chunks);
				const response = new Response(body.stream, {
					status: policy.status,
					headers: policy.status === 206 ? { 'content-range': 'bytes 0-1/2' } : {}
				});
				const arrayBuffer = vi.spyOn(response, 'arrayBuffer');
				const add = vi.fn();
				const remove = vi.fn();
				const controller = new SeekController('/reader-policy', async (_url, init) => {
					const added = vi.spyOn(init.signal, 'addEventListener');
					const removed = vi.spyOn(init.signal, 'removeEventListener');
					add.mockImplementation(() => added.mock.calls);
					remove.mockImplementation(() => removed.mock.calls);
					return response;
				});

				const pending = controller.seek(0, policy.end);
				if (trailingBytes && policy.exact) {
					await expect(pending).rejects.toThrow(
						'Range response body length does not match Content-Range'
					);
					expect(controller.completed).toBe(0);
				} else {
					const result = await pending;
					expect(result.bytes).toEqual(
						new Uint8Array(trailingBytes && !policy.bounded ? [1, 2, 3, 4] : [1, 2])
					);
					expect(controller.completed).toBe(1);
				}
				expect(controller.pending).toBe(false);
				expect(body.stream.locked).toBe(false);
				expect(arrayBuffer).not.toHaveBeenCalled();
				expect(add()).toEqual([['abort', expect.any(Function), { once: true }]]);
				expect(remove()).toEqual([['abort', add()[0][1]]]);
				if (policy.bounded && !policy.exact) {
					expect(body.pulls()).toBe(1);
					expect(body.cancel).toHaveBeenCalledTimes(1);
				} else if (policy.bounded && trailingBytes) {
					expect(body.pulls()).toBe(4);
					expect(body.cancel).toHaveBeenCalledTimes(1);
				} else {
					expect(body.pulls()).toBe(trailingBytes ? 5 : 4);
					expect(body.cancel).not.toHaveBeenCalled();
				}
			});
		}

		it(`${policy.name} preserves null-body fallback validation`, async () => {
			const response = new Response(null, {
				status: policy.status,
				headers: policy.status === 206 ? { 'content-range': 'bytes 0-1/2' } : {}
			});
			const arrayBuffer = vi
				.spyOn(response, 'arrayBuffer')
				.mockResolvedValue(new Uint8Array([1, 2, 3, 4]).buffer);
			const controller = new SeekController('/reader-null-body', async () => response);
			const pending = controller.seek(0, policy.end);
			if (policy.exact) {
				await expect(pending).rejects.toThrow(
					'Range response body length does not match Content-Range'
				);
				expect(controller.completed).toBe(0);
			} else {
				expect((await pending).bytes).toEqual(
					new Uint8Array(policy.bounded ? [1, 2] : [1, 2, 3, 4])
				);
				expect(controller.completed).toBe(1);
			}
			expect(arrayBuffer).toHaveBeenCalledTimes(1);
			expect(controller.pending).toBe(false);
		});

		it(`${policy.name} cancels and unlocks a stalled reader on abort`, async () => {
			let ready!: () => void;
			const reading = new Promise<void>((resolve) => {
				ready = resolve;
			});
			const cancel = vi.fn();
			const stream = new ReadableStream<Uint8Array>(
				{ pull: () => ready(), cancel },
				{ highWaterMark: 0 }
			);
			const controller = new SeekController('/reader-abort', async () =>
				new Response(stream, {
					status: policy.status,
					headers: policy.status === 206 ? { 'content-range': 'bytes 0-1/2' } : {}
				})
			);
			const pending = controller.seek(0, policy.end);
			const rejected = expect(pending).rejects.toMatchObject({ name: 'AbortError' });
			await reading;
			controller.abort();
			await rejected;
			expect(cancel).toHaveBeenCalledTimes(1);
			expect(stream.locked).toBe(false);
			expect(controller.pending).toBe(false);
			expect(controller.completed).toBe(0);
		});
	}
});
