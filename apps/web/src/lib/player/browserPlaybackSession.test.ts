import { expect, onTestFinished, test, vi } from 'vitest';
import { captureSystemActions, expireInsideRequestWindow, grantReply, initial, refusedContinuations, setup, setupToggleCase } from './fixtures/browserPlayback.js';
import { get } from 'svelte/store';
test('new plays use the acknowledged mode in the same tab and continuations keep the established source', async () => {
	const system = captureSystemActions();
	const { player, media, queue, fetchFn, starts, refresh, src, attach, delayGrant, toggleDuringFirstPlay, requests } = setup();
	media.captureShareSeek(media.beginShareInput(), { trackId: 'first', seconds: 18 }, () => true);
	expect([player.privateListening?.enabled, src(), fetchFn.mock.calls.length, media.sharePosition()]).toEqual([false, '', 0, 0]);
	await toggleDuringFirstPlay();
	const first = src(), tab = first.split('/')[5];
	expect([first, media.sharePosition()]).toEqual([`/v1/plays/media/first/${tab}/play-1`, 18]);
	const audio = attach();
	expect(player.privateListening?.enabled).toBe(true);
	refresh();
	await media.playCurrent();
	expect([src(), audio.play.mock.calls.length, fetchFn.mock.calls.length]).toEqual([first, 1, 2]);
	audio.failAt(36);
	for (const response of refusedContinuations(tab)) {
		fetchFn.mockResolvedValueOnce(response);
		await media.retryCurrent();
		expect([src(), audio.currentTime, audio.load.mock.calls.length]).toEqual([first, 36, 0]);
	}
	await media.retryCurrent();
	expect([src(), audio.currentTime, audio.load.mock.calls.length]).toEqual([first, 36, 1]);
	expect(requests().slice(2).map((body) => body.play_id)).toEqual(Array(3).fill('play-1'));
	media.sleepTimer.start({ kind: 'duration', ms: 60_000 }, 'Armed');
	const successor = delayGrant();
	system.get('nexttrack')!();
	await successor.started;
	const pending = { source: src(), audio: media.audio, request: requests().at(-1) };
	successor.reply(undefined);
	expect(pending.source).toBe(first);
	expect(pending.audio).toBe(audio);
	expect(pending.request).toEqual({ track_id: 'second', session_id: tab });
	await vi.waitFor(() => expect([src(), media.audio?.paused, get(media.sleepTimer).armed]).toEqual([`/v1/plays/media/second/${tab}/play-2`, false, true]));
	expect(queue.snapshot.tracks).toEqual(initial.tracks);
	await player.setPrivateListening(false);
	media.setRepeat('queue', null);
	attach(false).finish();
	await vi.waitFor(() => expect(src()).toBe(`/v1/plays/media/first/${tab}/play-3`));
	expect(starts).toEqual([false, true, false]);
	expect(requests().map((body) => body.session_id)).toEqual(Array(8).fill(tab));
});

test('a settled zero position takes precedence over a later failed decoder reading', async () => {
	const { media, src, play, attach } = setup();
	await play(0);
	const original = src();
	const audio = Object.assign(attach(), { seekable: { length: 0 } });
	await media.playCurrent();
	audio.dispatchEvent(new Event('timeupdate'));
	expect(get(media.absoluteSeek)).toMatchObject({ duration: 120, elapsed: 0 });

	audio.failAt(0.2);
	await media.retryCurrent();
	expect([src(), audio.currentTime, audio.load.mock.calls.length]).toEqual([original, 0, 1]);
});

test.each(['refused', 'superseded', 'account retirement', 'legacy reply', 'extra field', 'newer play'] as const)('%s fences grant publication', async (caseName) => {
	const { player, media, queue, fetchFn, lifetime, src, play, refresh, attach, delayGrant } = setup();
	await play(0);
	const incumbent = src(), snapshot = queue.snapshot;
	const audio = attach(false);
	fetchFn.mockResolvedValueOnce(new Response(null, { status: 403 }));
	await player.setPrivateListening(true);
	expect(player.privateListening?.enabled).toBe(false);
	const delayed = delayGrant();
	const playing = play(1);
	await delayed.started;
	if (caseName === 'superseded') refresh();
	if (caseName === 'account retirement') lifetime.abort();
	const selected: number[] = [];
	onTestFinished(queue.subscribe((value) => selected.push(value.currentIndex)));
	const latest = caseName === 'newer play' ? play(0) : undefined;
	delayed.reply(grantReply(caseName, incumbent.split('/')[5]));
	await Promise.all([playing, latest]);
	if (latest) {
		expect(selected).not.toContain(1);
		expect(src()).toMatch(/\/first\/[^/]+\/play-2$/);
	} else if (caseName === 'account retirement') {
		expect([player.privateListening, queue.snapshot.tracks.length, audio.pause.mock.calls.length]).toEqual([undefined, 0, 1]);
	} else {
		expect(queue.snapshot).toBe(snapshot);
		expect([src(), media.audio, audio.pause.mock.calls.length]).toEqual([incumbent, audio, 0]);
	}
});

test.each(['wrong tab', 'server error', 'short expiry', 'TTL expiry'] as const)('%s requires a fresh privacy acknowledgement before another start', async (reason) => {
	const { player, queue, fetchFn, src, play } = await setupToggleCase(reason);
	const incumbent = src(), snapshot = queue.snapshot;
	await player.setPrivateListening(true);
	expect(player.privateListening?.enabled).toBe(reason.endsWith('expiry'));
	if (reason.endsWith('expiry')) expireInsideRequestWindow(reason === 'TTL expiry' ? 21_600_000 : 60_000);
	await play(1);
	expect(queue.snapshot).toBe(snapshot);
	expect([fetchFn.mock.calls.length, src()]).toEqual([2, incumbent]);
});

test('existing track stays closed without synthetic fallback until verified grant arrives, then reopens cleanly', async () => {
	const { player, media, fetchFn, src, play, requests } = setup();
	const syntheticTracks = initial.tracks.map((track) => ({ ...track, streamUrl: '/api/demo/audio' }));
	expect(player.reconcileLibrary(syntheticTracks, () => true)).toBe(true);
	expect(player.replaceQueue({ tracks: syntheticTracks, currentIndex: 0 })).toBe(true);
	expect([src(), media.audio]).toEqual(['', undefined]);

	let sessionTab = '';
	fetchFn.mockImplementationOnce(async (_path, init) => {
		sessionTab = JSON.parse(String(init?.body)).session_id;
		return new Response(null, { status: 404 });
	});
	await play(0);
	expect([src(), media.audio, player.queueAnnouncement.text]).toEqual(['', undefined, 'Could not start playback.']);
	expect(sessionTab).toBeTruthy();

	const valid = { sessionBinding: 'A'.repeat(43), playbackSessionId: sessionTab, trackId: 'first', playId: 'play-1', durationMs: 120_000 };
	for (const reply of [
		new Response(null, { status: 503 }),
		new Response('not-json', { status: 200 }),
		Response.json({ play_id: 'legacy-1', grant: 'synthetic' }),
		Response.json({ sessionBinding: valid.sessionBinding, playbackSessionId: sessionTab, trackId: 'first', playId: 'play-1' }),
		Response.json({ ...valid, durationMs: 0 }),
		Response.json({ ...valid, playId: '' }),
		Response.json({ ...valid, trackId: 'second' }),
		Response.json({ ...valid, playbackSessionId: 'other-tab' }),
		Response.json({ ...valid, sessionBinding: 'B'.repeat(43) }),
		Response.json({ ...valid, extra: 'unexpected' }),
	]) {
		fetchFn.mockResolvedValueOnce(reply);
		await play(0);
		expect([src(), media.audio]).toEqual(['', undefined]);
	}

	await play(0);
	const expectedSrc = `/v1/plays/media/first/${sessionTab}/play-1`;
	expect([src(), media.audio?.getAttribute('src')]).toEqual([expectedSrc, expectedSrc]);
	expect(requests().every((body) => body.track_id === 'first' && body.session_id === sessionTab)).toBe(true);
});
