import { expect, it, onTestFinished, vi } from 'vitest';
import type { AccountSession } from '../auth-fence.js';
import { createBrowserPlaybackGrant, type BrowserPlaybackSource } from './browserPlaybackGrant.js';
import { createListenEventSender } from './listenEventSender.js';
import { observeListenEvents } from './observeListenEvents.js';

it('orders the current grant’s events and refuses the retired grant’s late terminal', async () => {
	vi.useFakeTimers();
	onTestFinished(() => vi.useRealTimers());
	const account = new AbortController();
	const owner: AccountSession = {
		generation: 'synthetic-account', signal: account.signal, isCurrent: () => !account.signal.aborted,
		run: async (work) => !account.signal.aborted ? work() : undefined
	};
	const trackId = 'synthetic-track';
	const context = 'B'.repeat(42) + 'E';
	const pendingStart = Promise.withResolvers<Response>();
	const ack = () => Response.json({ ok: true });
	const fetchFn = vi.fn<typeof fetch>()
		.mockResolvedValueOnce(Response.json({ context }))
		.mockResolvedValueOnce(ack())
		.mockImplementationOnce(() => pendingStart.promise)
		.mockImplementation(async () => ack());
	const writes = () => fetchFn.mock.calls.filter(([path]) => path === '/v1/plays/events')
		.map(([, init]) => JSON.parse(String(init?.body)));

	let source: BrowserPlaybackSource | undefined;
	const grant = createBrowserPlaybackGrant(owner, 'A'.repeat(43), async (request) => ({
		...request, durationMs: 60_000
	}));
	const sender = createListenEventSender({ owner, canRecord: () => true, grantForStart: () => source, fetchFn });
	onTestFinished(() => { sender.dispose(); grant.dispose(); });
	await sender.ready;

	const startedPlaybacks = new Set<string>();
	const observerOptions = { trackId, sender, startedPlaybacks, grantPlayId: () => source?.playId };
	const retiredMedia = new EventTarget();
	const currentMedia = new EventTarget();
	const retiredObserver = observeListenEvents(retiredMedia, observerOptions);
	const currentObserver = observeListenEvents(currentMedia, observerOptions);
	onTestFinished(() => { retiredObserver.dispose(); currentObserver.dispose(); });
	const retiredGrant = new AbortController();
	const open = (playId: string, signal: AbortSignal) =>
		grant.open({ trackId, playId }, signal, () => true, (issued) => { source = issued; });

	// The expected IDs spell out the wire encoding independently of the observer.
	const retiredStart = { playback_id: '40c325c0-2cee-489d-a8b6-fbe974155601', track_id: trackId, event: 'start', context };
	const currentStart = { ...retiredStart, playback_id: '40c325c0-2cee-489d-a8b6-fbe974155602' };
	expect(await open('QMMlwCzuSJ2otvvpdBVWAQ', retiredGrant.signal)).toBe(true);
	retiredMedia.dispatchEvent(new Event('playing'));
	await vi.advanceTimersByTimeAsync(0);
	expect(writes()).toEqual([retiredStart]);

	retiredGrant.abort();
	expect(await open('QMMlwCzuSJ2otvvpdBVWAg', account.signal)).toBe(true);
	currentMedia.dispatchEvent(new Event('playing'));
	// Both elements are for the same track. A terminal that re-reads the grant
	// could preempt the replacement's skip with the retired element's complete.
	retiredMedia.dispatchEvent(new Event('ended'));
	currentMedia.dispatchEvent(new Event('emptied'));
	await vi.advanceTimersByTimeAsync(0);
	expect(writes()).toEqual([retiredStart, currentStart]);

	pendingStart.resolve(ack());
	await vi.advanceTimersByTimeAsync(0);
	expect(writes()).toEqual([retiredStart, currentStart, { ...currentStart, event: 'skip' }]);
});
