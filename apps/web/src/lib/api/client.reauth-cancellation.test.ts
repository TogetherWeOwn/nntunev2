import { expect, it } from 'vitest';
import { ApiClient } from './client.js';

for (const phase of ['start', 'finish'] as const) {
	it(`forwards caller cancellation through reauthentication ${phase}`, async () => {
		let requestSignal: AbortSignal | null = null;
		let settle!: (value: Response) => void;
		const client = new ApiClient({
			baseUrl: 'https://api.example.test',
			fetchFn: ((_url, init) => {
				requestSignal = init?.signal ?? null;
				return new Promise<Response>((resolve, reject) => {
					settle = resolve;
					requestSignal?.addEventListener('abort', () => reject(requestSignal?.reason), { once: true });
				});
			}) as typeof fetch
		});
		const controller = new AbortController();
		const result = (phase === 'start'
			? client.reauthenticateStart(controller.signal)
			: client.reauthenticateFinish({
				challenge: 'YQ', credentialId: 'fixture', authenticatorData: 'Aw', clientDataJson: 'Ag', signature: 'BA'
			}, controller.signal)).catch((error: unknown) => error);
		controller.abort(new DOMException('fixture navigation', 'AbortError'));
		// Settle the synthetic transport even when testing an implementation
		// that fails to forward cancellation, so RED leaves no hanging timer.
		settle(new Response(JSON.stringify(phase === 'start' ? { challenge: 'YQ', rp_id: 'example.test' } : { ok: true })));
		const received = await result;
		expect((requestSignal as AbortSignal | null)?.aborted).toBe(true);
		expect(received).toBeInstanceOf(DOMException);
		expect((received as DOMException).name).toBe('AbortError');
	});
}
