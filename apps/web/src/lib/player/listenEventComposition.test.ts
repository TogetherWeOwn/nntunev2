import { expect, it, onTestFinished, vi } from 'vitest';
import type { AccountSession } from '../auth-fence.js';
import { LIBRARY_AUTH_TRANSITION_KEY } from '../library/cache.js';
import { createBrowserPlaybackGrant, type BrowserPlaybackSource } from './browserPlaybackGrant.js';
import { createListenEventSender } from './listenEventSender.js';
import { observeListenEvents } from './observeListenEvents.js';
import { createPlayerListenHistory } from './playerListenHistory.js';

const trackId = 'synthetic-track';
const context = 'B'.repeat(42) + 'E';
const eventsPath = '/v1/plays/events';
const contextPath = `${eventsPath}/context`;
const ack = () => Response.json({ ok: true });

function listenTransport(fetchFn: typeof fetch) {
	vi.useFakeTimers();
	onTestFinished(() => { vi.useRealTimers(); });
	const account = new AbortController();
	const owner: AccountSession = {
		generation: 'synthetic-account', signal: account.signal, isCurrent: () => !account.signal.aborted,
		run: async (work) => !account.signal.aborted ? work() : undefined
	};
	let source: BrowserPlaybackSource | undefined;
	const grant = createBrowserPlaybackGrant(owner, 'A'.repeat(43), async (request) => ({
		...request, durationMs: 60_000
	}));
	const sender = createListenEventSender({ owner, canRecord: () => true, grantForStart: () => source, fetchFn });
	onTestFinished(() => { sender.dispose(); grant.dispose(); });
	return {
		sender,
		currentPlayId: () => source?.playId,
		open: (playId: string, signal = account.signal) =>
			grant.open({ trackId, playId }, signal, () => true, (issued) => { source = issued; })
	};
}

it('orders the current grant’s events and refuses the retired grant’s late terminal', async () => {
	const pendingStart = Promise.withResolvers<Response>();
	const fetchFn = vi.fn<typeof fetch>()
		.mockResolvedValueOnce(Response.json({ context }))
		.mockResolvedValueOnce(ack())
		.mockImplementationOnce(() => pendingStart.promise)
		.mockImplementation(async () => ack());
	const writes = () => fetchFn.mock.calls.filter(([path]) => path === eventsPath)
		.map(([, init]) => JSON.parse(String(init?.body)));
	const { sender, currentPlayId, open } = listenTransport(fetchFn);
	await sender.ready;

	const startedPlaybacks = new Set<string>();
	const observerOptions = { trackId, sender, startedPlaybacks, grantPlayId: currentPlayId };
	const retiredMedia = new EventTarget();
	const currentMedia = new EventTarget();
	const retiredObserver = observeListenEvents(retiredMedia, observerOptions);
	const currentObserver = observeListenEvents(currentMedia, observerOptions);
	onTestFinished(() => { retiredObserver.dispose(); currentObserver.dispose(); });
	const retiredGrant = new AbortController();

	// The expected IDs spell out the wire encoding independently of the observer.
	const retiredStart = { playback_id: '40c325c0-2cee-489d-a8b6-fbe974155601', track_id: trackId, event: 'start', context };
	const currentStart = { ...retiredStart, playback_id: '40c325c0-2cee-489d-a8b6-fbe974155602' };
	expect(await open('QMMlwCzuSJ2otvvpdBVWAQ', retiredGrant.signal)).toBe(true);
	retiredMedia.dispatchEvent(new Event('playing'));
	await vi.advanceTimersByTimeAsync(0);
	expect(writes()).toEqual([retiredStart]);

	retiredGrant.abort();
	expect(await open('QMMlwCzuSJ2otvvpdBVWAg')).toBe(true);
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

it('reuses the listen context on page restore and stays closed after an auth transition', async () => {
	const fetchFn = vi.fn<typeof fetch>(async (path) => path === contextPath ? Response.json({ context }) : ack());
	const { sender, currentPlayId, open } = listenTransport(fetchFn);
	const audio = new EventTarget() as HTMLAudioElement;
	const lifecycle = new EventTarget() as Window;
	const history = createPlayerListenHistory(sender, lifecycle);
	onTestFinished(() => history.resetAtAuthBoundary());
	history.attach(audio, trackId, currentPlayId);
	await sender.ready;

	const transition = (type: 'pagehide' | 'pageshow') =>
		lifecycle.dispatchEvent(Object.assign(new Event(type), { persisted: true }));
	const playing = () => audio.dispatchEvent(new Event('playing'));
	const requests = () => fetchFn.mock.calls.map(([path, init]) =>
		[path, JSON.parse(String(init?.body ?? 'null'))]);
	const first = { playback_id: '00000000-0000-0000-0000-000000000000', track_id: trackId, event: 'start', context };
	const restored = { ...first, playback_id: '00000000-0000-0000-0000-000000000001' };
	const start = [[contextPath, null], [eventsPath, first]];
	const finished = [...start, [eventsPath, { ...first, event: 'skip' }]];
	const expected = [...finished, [eventsPath, restored]];

	expect(await open('A'.repeat(22))).toBe(true);
	playing();
	await vi.advanceTimersByTimeAsync(0);
	expect(requests()).toEqual(start);
	transition('pagehide');
	await vi.advanceTimersByTimeAsync(0);
	expect(requests()).toEqual(finished);

	transition('pageshow');
	expect(await open('A'.repeat(21) + 'Q')).toBe(true);
	playing();
	await vi.advanceTimersByTimeAsync(0);
	expect(requests()).toEqual(expected);

	lifecycle.dispatchEvent(Object.assign(new Event('storage'), { key: LIBRARY_AUTH_TRANSITION_KEY }));
	transition('pagehide');
	transition('pageshow');
	expect(await open('A'.repeat(21) + 'g')).toBe(true);
	playing();
	await vi.advanceTimersByTimeAsync(0);
	expect(requests()).toEqual(expected);

	// Exercise the sender directly because the closed observer cannot forward a start.
	sender.send({ playback_id: '00000000-0000-0000-0000-000000000002', track_id: trackId, event: 'start' });
	await vi.advanceTimersByTimeAsync(0);
	expect(requests()).toEqual(expected);
});
