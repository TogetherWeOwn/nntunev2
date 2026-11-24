import { expect, onTestFinished, test, vi } from 'vitest';
import { expireInsideRequestWindow, grantReply, initial, refusedContinuations, setup } from './fixtures/browserPlayback.js';

test('new plays use the acknowledged mode in the same tab and continuations keep the established source', async () => {
	const { player, media, queue, fetchFn, starts, refresh, src, attach, mountOnFlush } = setup();
	expect(player.privateListening?.enabled).toBe(false);
	media.captureShareSeek(media.beginShareInput(), { trackId: 'first', seconds: 18 }, () => true);
	mountOnFlush();
	expect([src(), fetchFn.mock.calls.length]).toEqual(['', 0]);
	await media.playCurrent();
	const first = src(), tab = first.split('/')[5];
	expect([first, media.audio?.currentTime]).toEqual([`/v1/plays/media/first/${tab}/play-1`, 18]);
	const audio = attach();
	await player.setPrivateListening(true);
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
	expect(fetchFn.mock.calls.slice(2).map(([, init]) => JSON.parse(String(init?.body)).play_id)).toEqual(Array(3).fill('play-1'));
	media.next();
	await vi.waitFor(() => expect(src()).toBe(`/v1/plays/media/second/${tab}/play-2`));
	expect(queue.snapshot.tracks).toEqual(initial.tracks);
	await player.setPrivateListening(false);
	media.setRepeat('queue', null);
	attach(false).finish();
	await vi.waitFor(() => expect(src()).toBe(`/v1/plays/media/first/${tab}/play-3`));
	expect(starts).toEqual([false, true, false]);
	expect(fetchFn.mock.calls.map(([, init]) => JSON.parse(String(init?.body)).session_id)).toEqual(Array(8).fill(tab));
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
		expect(player.privateListening).toBeUndefined();
		expect(queue.snapshot.tracks).toHaveLength(0);
		expect(audio.pause).toHaveBeenCalledOnce();
	} else {
		expect(queue.snapshot).toBe(snapshot);
		expect(src()).toBe(incumbent);
		expect([media.audio === audio, audio.pause.mock.calls.length]).toEqual([true, 0]);
	}
});

test.each(['wrong tab', 'server error', 'expired'] as const)('%s requires a fresh privacy acknowledgement before another start', async (reason) => {
	const { player, queue, fetchFn, src, play } = setup(reason === 'expired' ? 7 * 60 * 60 : 60);
	await play(0);
	const incumbent = src(), snapshot = queue.snapshot;
	if (reason === 'wrong tab') fetchFn.mockResolvedValueOnce(Response.json({ session_id: 'another-tab', private_until: Math.floor(Date.now() / 1000) + 60 }));
	if (reason === 'server error') fetchFn.mockResolvedValueOnce(new Response(null, { status: 503 }));
	await player.setPrivateListening(true);
	expect(player.privateListening?.enabled).toBe(reason === 'expired');
	if (reason === 'expired') expireInsideRequestWindow();
	await play(1);
	expect(fetchFn).toHaveBeenCalledTimes(2);
	expect(queue.snapshot).toBe(snapshot);
	expect(src()).toBe(incumbent);
});
