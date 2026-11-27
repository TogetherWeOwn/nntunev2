import { expect, it, onTestFinished, vi } from 'vitest';
import type { AccountSession } from '../auth-fence.js';
import { LIBRARY_AUTH_TRANSITION_KEY } from '../library/cache.js';
import { createBrowserPlaybackGrant, type BrowserPlaybackSource } from './browserPlaybackGrant.js';
import { createPlayerController } from './controller.svelte.js';
import { createListenEventSender } from './listenEventSender.js';
import { observeListenEvents } from './observeListenEvents.js';
import { createPlayerListenHistory } from './playerListenHistory.js';
import { createQueue, currentTrack } from './queue.js';

const trackId = 'synthetic-track';
const context = 'B'.repeat(42) + 'E';
const eventsPath = '/v1/plays/events';
const contextPath = `${eventsPath}/context`;
const ack = () => Response.json({ ok: true });
const listenMedia = () => Object.assign(new EventTarget(), { paused: false, seeking: false, ended: false, error: null });

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
		data: { ...request, durationMs: 60_000 }
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
	const retiredMedia = listenMedia();
	const currentMedia = listenMedia();
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
	retiredMedia.ended = true;
	retiredMedia.dispatchEvent(new Event('ended'));
	currentMedia.dispatchEvent(new Event('emptied'));
	await vi.advanceTimersByTimeAsync(0);
	expect(writes()).toEqual([retiredStart, currentStart]);

	pendingStart.resolve(ack());
	await vi.advanceTimersByTimeAsync(0);
	expect(writes()).toEqual([retiredStart, currentStart, { ...currentStart, event: 'skip' }]);
});

it('keeps the listen on pause and posts one skip for abandonment or media error', async () => {
	const fetchFn = vi.fn<typeof fetch>(async (path) => path === contextPath ? Response.json({ context }) : ack());
	const writes = () => fetchFn.mock.calls.filter(([path]) => path === eventsPath)
		.map(([, init]) => JSON.parse(String(init?.body)));
	const { sender, currentPlayId, open } = listenTransport(fetchFn);
	const audio = listenMedia() as unknown as HTMLAudioElement;
	const history = createPlayerListenHistory(sender, new EventTarget() as Window);
	onTestFinished(() => history.resetAtAuthBoundary());
	history.attach(audio, trackId, currentPlayId);
	await sender.ready;

	const fire = (event: string) => audio.dispatchEvent(new Event(event));
	const first = { playback_id: '00000000-0000-0000-0000-000000000000', track_id: trackId, event: 'start', context };
	const replay = { ...first, playback_id: '00000000-0000-0000-0000-000000000001' };
	expect(await open('A'.repeat(22))).toBe(true);
	fire('playing');
	await vi.advanceTimersByTimeAsync(0);
	expect(writes()).toEqual([first]);

	fire('pause');
	await vi.advanceTimersByTimeAsync(0);
	expect(writes()).toEqual([first]);
	fire('playing');
	await vi.advanceTimersByTimeAsync(0);
	expect(writes()).toEqual([first]);

	history.finish(audio);
	const abandoned = [first, { ...first, event: 'skip' }];
	await vi.advanceTimersByTimeAsync(0);
	expect(writes()).toEqual(abandoned);
	fire('playing');
	await vi.advanceTimersByTimeAsync(0);
	expect(writes()).toEqual(abandoned);

	expect(await open('A'.repeat(21) + 'Q')).toBe(true);
	fire('playing');
	await vi.advanceTimersByTimeAsync(0);
	expect(writes()).toEqual([...abandoned, replay]);

	const failed = [...abandoned, replay, { ...replay, event: 'skip' }];
	fire('error');
	await vi.advanceTimersByTimeAsync(0);
	expect(writes()).toEqual(failed);
	fire('error');
	fire('emptied');
	fire('playing');
	await vi.advanceTimersByTimeAsync(0);
	expect(writes()).toEqual(failed);
});

it('reuses the listen context on page restore and stays closed after an auth transition', async () => {
	const fetchFn = vi.fn<typeof fetch>(async (path) => path === contextPath ? Response.json({ context }) : ack());
	const { sender, currentPlayId, open } = listenTransport(fetchFn);
	const audio = listenMedia() as unknown as HTMLAudioElement;
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

const binding = 'A'.repeat(43);
const transitionTracks = ['track-a', 'track-b', 'track-c'].map((id) => ({ id, title: id, artist: '', streamUrl: `/fixture/${id}` }));
const grantPairs = [
	['QMMlwCzuSJ2otvvpdBVWAQ', '40c325c0-2cee-489d-a8b6-fbe974155601'],
	['QMMlwCzuSJ2otvvpdBVWAg', '40c325c0-2cee-489d-a8b6-fbe974155602'],
	['QMMlwCzuSJ2otvvpdBVWAw', '40c325c0-2cee-489d-a8b6-fbe974155603']
] as const;

type SyntheticAudio = EventTarget & {
	isConnected: boolean; paused: boolean; ended: boolean; seeking: boolean; error: MediaError | null;
	currentTime: number; duration: number; seekable: { length: number; start: () => number; end: () => number };
	getAttribute: () => string; play: ReturnType<typeof vi.fn>; pause: ReturnType<typeof vi.fn>; load: ReturnType<typeof vi.fn>;
};
const fire = (target: EventTarget, ...types: string[]) => { for (const type of types) target.dispatchEvent(new Event(type)); };

async function transitionFixture(onEvent?: (body: { playback_id: string; track_id: string; event: string }) => Promise<Response> | Response) {
	vi.useFakeTimers();
	onTestFinished(() => { vi.useRealTimers(); });
	const account = new AbortController();
	const owner: AccountSession = {
		generation: 'synthetic-account', signal: account.signal, isCurrent: () => !account.signal.aborted,
		run: async (work) => !account.signal.aborted ? work() : undefined
	};
	const grantTracks: string[] = [];
	const eventWrites: Array<{ playback_id: string; track_id: string; event: string; context: string }> = [];
	let grantIndex = 0;
	let delayedGrant: { started: PromiseWithResolvers<void>; release: PromiseWithResolvers<Response | undefined> } | undefined;

	const grantFetch = vi.fn<typeof fetch>(async (path, init) => {
		expect(path).toBe('/v1/plays');
		const body = JSON.parse(String(init?.body)) as { track_id: string; session_id: string; play_id?: string };
		grantTracks.push(body.track_id);
		const reply = () => Response.json({
			sessionBinding: binding, playbackSessionId: body.session_id, trackId: body.track_id,
			playId: body.play_id ?? grantPairs[grantIndex++]![0], durationMs: 120_000
		});
		if (!delayedGrant) return reply();
		const pending = delayedGrant;
		delayedGrant = undefined;
		pending.started.resolve();
		return (await pending.release.promise) ?? reply();
	});
	const listenFetch = vi.fn<typeof fetch>(async (path, init) => {
		if (path === contextPath) return Response.json({ context });
		const body = JSON.parse(String(init?.body));
		eventWrites.push(body);
		return onEvent ? await onEvent(body) : ack();
	});

	let mounted: SyntheticAudio | undefined, mountedRevision = -1, detachHistory: (() => void) | undefined;
	const activeSource = (): BrowserPlaybackSource | undefined => {
		const track = currentTrack(player.queue.snapshot), streamUrl = player.streamFor(track), playId = streamUrl.split('/')[6];
		return track && streamUrl && playId ? { streamUrl, trackId: track.id, playId, isCurrent: owner.isCurrent } : undefined;
	};
	const mountCurrent = () => {
		const source = activeSource();
		if (mountedRevision === player.mediaRevision && mounted && source) return;
		detachHistory?.(); detachHistory = undefined;
		if (mounted) mounted.isConnected = false;
		mounted = undefined; mountedRevision = player.mediaRevision;
		if (!source) return;
		const audio: SyntheticAudio = Object.assign(new EventTarget(), {
			isConnected: true, paused: true, ended: false, seeking: false, error: null as MediaError | null,
			currentTime: 0, duration: 120, seekable: { length: 1, start: () => 0, end: () => 120 }, getAttribute: () => source.streamUrl,
			play: vi.fn(async () => { audio.paused = false; }), pause: vi.fn(() => { audio.paused = true; }), load: vi.fn()
		});
		mounted = audio;
		player.media.attach(audio as unknown as HTMLAudioElement);
		detachHistory = history.attach(audio as unknown as HTMLAudioElement, source.trackId,
			() => activeSource()?.streamUrl === source.streamUrl ? activeSource()?.playId : undefined);
	};
	const player = createPlayerController(createQueue(transitionTracks), { flush: async () => { mountCurrent(); } },
		(node, src) => node.isConnected && node.getAttribute('src') === src, { fetchFn: grantFetch });
	const sender = createListenEventSender({ owner, canRecord: () => true, grantForStart: activeSource, fetchFn: listenFetch });
	const history = createPlayerListenHistory(sender, new EventTarget() as Window);
	onTestFinished(() => { detachHistory?.(); history.resetAtAuthBoundary(); sender.dispose(); player.destroy(); });
	player.bindBrowserPlayback(owner, binding);
	player.reconcileLibrary(transitionTracks, () => true);
	await sender.ready;

	const play = (index: number) =>
		player.media.playSelected(player.queue.snapshot, index, player.queue.sourceForQueuedPlay(transitionTracks[index]!.id)!);
	const delayNextGrant = () => {
		const pending = delayedGrant = { started: Promise.withResolvers<void>(), release: Promise.withResolvers<Response | undefined>() };
		return { started: pending.started.promise, release: (resp?: Response) => pending.release.resolve(resp) };
	};
	const flushWrites = async () => { for (let i = 0; i < 6; i++) await vi.advanceTimersByTimeAsync(0); };
	const row = (grant: number, track_id: string, event: 'start' | 'complete' | 'skip') =>
		({ playback_id: grantPairs[grant]![1], track_id, event, context });
	const end = (audio: SyntheticAudio, ...types: string[]) => { Object.assign(audio, { ended: true, paused: true }); fire(audio, ...types); };
	return { player, play, audio: () => mounted!, grantTracks, eventWrites, delayNextGrant, flushWrites, row, end };
}

it('requires current playback before consuming a grant', async () => {
	const f = await transitionFixture();
	await f.play(0);
	const audio = f.audio();
	audio.paused = true;
	fire(audio, 'play', 'canplay', 'playing');
	Object.assign(audio, { paused: false, seeking: true });
	fire(audio, 'seeking', 'playing');
	Object.assign(audio, { seeking: false, error: { code: 3 } });
	fire(audio, 'playing', 'error');
	Object.assign(audio, { error: null, ended: true });
	fire(audio, 'playing', 'ended');
	await f.flushWrites();
	expect(f.eventWrites).toEqual([]);

	audio.ended = false;
	fire(audio, 'playing', 'playing');
	await f.flushWrites();
	expect(f.eventWrites).toEqual([f.row(0, 'track-a', 'start')]);
});

it('emits one start and complete per track across normal end transitions without double-crediting', async () => {
	const f = await transitionFixture();
	await f.play(0);
	const firstAudio = f.audio();
	fire(firstAudio, 'playing', 'playing');
	firstAudio.paused = true; fire(firstAudio, 'pause');
	firstAudio.paused = false; fire(firstAudio, 'playing');
	await f.flushWrites();
	expect(f.eventWrites).toEqual([f.row(0, 'track-a', 'start')]);

	f.end(firstAudio, 'pause', 'ended', 'ended');
	await f.flushWrites();
	// Late events on the retired element cannot double-credit track-a or leak into track-b.
	fire(firstAudio, 'ended', 'emptied', 'error', 'playing');
	const secondAudio = f.audio();
	expect(secondAudio).not.toBe(firstAudio);
	fire(secondAudio, 'playing');
	await f.flushWrites();
	expect(f.eventWrites).toEqual([
		f.row(0, 'track-a', 'start'), f.row(0, 'track-a', 'complete'), f.row(1, 'track-b', 'start')
	]);

	f.end(secondAudio, 'ended');
	await f.flushWrites();
	const thirdAudio = f.audio();
	fire(thirdAudio, 'playing');
	await f.flushWrites();
	f.end(thirdAudio, 'timeupdate', 'ended');
	await f.flushWrites();
	// Replaying the ended final track reuses its grant and must not emit a second start or complete.
	thirdAudio.ended = false;
	await f.player.media.playCurrent();
	fire(thirdAudio, 'playing');
	f.end(thirdAudio, 'ended');
	await f.flushWrites();
	expect(f.grantTracks).toEqual(['track-a', 'track-b', 'track-c']);
	expect(f.eventWrites).toEqual([
		f.row(0, 'track-a', 'start'), f.row(0, 'track-a', 'complete'),
		f.row(1, 'track-b', 'start'), f.row(1, 'track-b', 'complete'),
		f.row(2, 'track-c', 'start'), f.row(2, 'track-c', 'complete')
	]);
});

it('keeps one listen across seeks and clears sampled end when seeking back before abandonment', async () => {
	const f = await transitionFixture();
	await f.play(0);
	const firstAudio = f.audio();
	fire(firstAudio, 'playing');
	for (const target of [30, 75]) {
		Object.assign(firstAudio, { currentTime: target, seeking: true }); fire(firstAudio, 'seeking');
		firstAudio.seeking = false; fire(firstAudio, 'seeked', 'playing');
	}
	await f.flushWrites();
	expect(f.eventWrites).toEqual([f.row(0, 'track-a', 'start')]);

	// Seeking back invalidates the sampled end and any queued ended event.
	firstAudio.ended = true; fire(firstAudio, 'timeupdate');
	Object.assign(firstAudio, { ended: false, currentTime: 40 });
	fire(firstAudio, 'seeking', 'seeked', 'ended');
	await f.flushWrites();
	expect(f.eventWrites).toEqual([f.row(0, 'track-a', 'start')]);
	f.player.media.nextFromQueue();
	await f.flushWrites();

	const secondAudio = f.audio();
	fire(secondAudio, 'playing');
	secondAudio.currentTime = 60; fire(secondAudio, 'seeking', 'seeked');
	f.end(secondAudio, 'ended');
	await f.flushWrites();
	expect(f.eventWrites).toEqual([
		f.row(0, 'track-a', 'start'), f.row(0, 'track-a', 'skip'),
		f.row(1, 'track-b', 'start'), f.row(1, 'track-b', 'complete')
	]);
});

it.each([200, 409] as const)('prevents leaks across rapid skips over an unstarted track (first start %s)', async (firstStatus) => {
	const pendingFirst = Promise.withResolvers<Response>();
	const f = await transitionFixture((body) =>
		body.playback_id === grantPairs[0][1] && body.event === 'start' ? pendingFirst.promise : ack()
	);
	await f.play(0);
	fire(f.audio(), 'playing');
	await f.flushWrites();

	// Skip track-a while its start is in flight, then skip track-b before it ever plays.
	await f.play(1);
	const unstartedSecond = f.audio();
	fire(unstartedSecond, 'emptied');
	await f.play(2);
	fire(unstartedSecond, 'error');
	const thirdAudio = f.audio();
	fire(thirdAudio, 'playing');
	await f.flushWrites();

	pendingFirst.resolve(Response.json({ ok: firstStatus === 200 }, { status: firstStatus }));
	await f.flushWrites();
	f.end(thirdAudio, 'ended');
	await f.flushWrites();
	expect(f.eventWrites).toEqual([
		f.row(0, 'track-a', 'start'), f.row(2, 'track-c', 'start'),
		...(firstStatus === 200 ? [f.row(0, 'track-a', 'skip')] : []),
		f.row(2, 'track-c', 'complete')
	]);
});

it('does not double-credit or leak across an aborted track-end transition', async () => {
	const f = await transitionFixture();
	await f.play(0);
	const firstAudio = f.audio();
	fire(firstAudio, 'playing');
	await f.flushWrites();

	const delayedSuccessor = f.delayNextGrant();
	f.end(firstAudio, 'ended');
	await delayedSuccessor.started;
	await f.flushWrites();
	expect(f.eventWrites).toEqual([f.row(0, 'track-a', 'start'), f.row(0, 'track-a', 'complete')]);

	// Pause alone cancels the in-flight track-b transition before any later selection.
	f.player.media.pauseCurrent();
	delayedSuccessor.release();
	await f.flushWrites();
	expect([f.audio() === firstAudio, currentTrack(f.player.queue.snapshot)?.id]).toEqual([true, 'track-a']);
	expect(f.eventWrites).toEqual([f.row(0, 'track-a', 'start'), f.row(0, 'track-a', 'complete')]);

	await f.play(2);
	const thirdAudio = f.audio();
	fire(thirdAudio, 'playing');
	f.end(thirdAudio, 'ended');
	await f.flushWrites();
	expect(f.grantTracks).toEqual(['track-a', 'track-b', 'track-c']);
	expect(f.eventWrites).toEqual([
		f.row(0, 'track-a', 'start'), f.row(0, 'track-a', 'complete'),
		f.row(2, 'track-c', 'start'), f.row(2, 'track-c', 'complete')
	]);
});
