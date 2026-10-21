import { describe, expect, it, vi } from 'vitest';
import { ApiClient } from './client.js';
import { onPaywall } from '../billing/planLimit.js';

function deferred<T>() {
	let resolve!: (value: T) => void;
	const promise = new Promise<T>((done) => (resolve = done));
	return { promise, resolve };
}

describe('component-scoped API cancellation', () => {
	it('aborts the transport when its owner leaves', async () => {
		const scope = new AbortController();
		let transport: AbortSignal | null | undefined;
		const fetchFn: typeof fetch = vi.fn((_input, init) => {
			transport = init?.signal;
			return new Promise<Response>((_resolve, reject) => {
				transport?.addEventListener('abort', () => reject(transport?.reason), { once: true });
			});
		});
		const client = new ApiClient({ baseUrl: 'https://api.example.test', fetchFn, signal: scope.signal });
		const pending = client.logout().catch((error: unknown) => error);
		scope.abort();
		expect(transport?.aborted).toBe(true);
		expect(await pending).toBe(scope.signal.reason);
	});

	for (const status of [200, 401, 402, 429]) {
		it(`ignores a late ${status} response even if fetch ignores cancellation`, async () => {
			const scope = new AbortController();
			const response = deferred<Response>();
			const listener = vi.fn();
			const unsubscribe = onPaywall(listener);
			try {
				const client = new ApiClient({
					baseUrl: 'https://api.example.test',
					fetchFn: () => response.promise,
					signal: scope.signal
				});
				const pending = client.finishMagicLink('fixture-token').catch((error: unknown) => error);
				scope.abort();
				response.resolve(new Response(JSON.stringify({ ok: true }), { status }));
				expect(await pending).toBe(scope.signal.reason);
				expect(listener).not.toHaveBeenCalled();
			} finally {
				unsubscribe();
			}
		});
	}

	for (const status of [200, 401, 402, 429]) {
		it(`ignores a ${status} body that finishes after its owner leaves`, async () => {
			const scope = new AbortController();
			const reading = deferred<void>();
			let stream!: ReadableStreamDefaultController<Uint8Array>;
			const body = new ReadableStream<Uint8Array>({
				start(controller) { stream = controller; },
				pull() { reading.resolve(); }
			}, { highWaterMark: 0 });
			const listener = vi.fn();
			const unsubscribe = onPaywall(listener);
			try {
				const client = new ApiClient({
					baseUrl: 'https://api.example.test',
					fetchFn: async () => new Response(body, { status }),
					signal: scope.signal
				});
				const pending = client.finishMagicLink('fixture-token').catch((error: unknown) => error);
				await reading.promise;
				scope.abort();
				stream.enqueue(new TextEncoder().encode(JSON.stringify({ ok: true })));
				stream.close();
				expect(await pending).toBe(scope.signal.reason);
				expect(listener).not.toHaveBeenCalled();
			} finally {
				unsubscribe();
			}
		});
	}

	it('does not start a request from an already-abandoned client', async () => {
		const scope = new AbortController();
		scope.abort();
		const fetchFn = vi.fn(async () => new Response('{"ok":true}'));
		const client = new ApiClient({ baseUrl: 'https://api.example.test', fetchFn, signal: scope.signal });
		await expect(client.logout()).rejects.toBe(scope.signal.reason);
		expect(fetchFn).not.toHaveBeenCalled();
	});
});
