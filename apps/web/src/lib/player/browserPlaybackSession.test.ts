import { expect, onTestFinished, test, vi } from 'vitest';
import { grantReply, initial, setup } from './fixtures/browserPlayback.js';

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
	audio.currentTime = 36;
	audio.error = { code: 2 } as MediaError;
	audio.dispatchEvent(new Event('error'));
	await media.retryCurrent();
	expect([src(), audio.currentTime, audio.load.mock.calls.length]).toEqual([first, 36, 1]);
	expect(JSON.parse(String(fetchFn.mock.lastCall?.[1]?.body)).play_id).toBe('play-1');
	media.next();
	await vi.waitFor(() => expect(src()).toBe(`/v1/plays/media/second/${tab}/play-2`));
	expect(queue.snapshot.tracks).toEqual(initial.tracks);
	await player.setPrivateListening(false);
	const ending = attach(false);
	ending.dispatchEvent(new Event('play'));
	ending.ended = true;
	media.setRepeat('queue', null);
	ending.dispatchEvent(new Event('ended'));
	await vi.waitFor(() => expect(src()).toBe(`/v1/plays/media/first/${tab}/play-3`));
	expect(starts).toEqual([false, true, false]);
	expect(fetchFn.mock.calls.map(([, init]) => JSON.parse(String(init?.body)).session_id)).toEqual(Array(6).fill(tab));
});

test.each(['refused', 'superseded', 'account retirement', 'legacy reply', 'extra field', 'newer play'] as const)('%s fences grant publication', async (caseName) => {
	const { player, media, queue, fetchFn, lifetime, src, play, refresh, attach } = setup();
	await play(0);
	const incumbent = src(), snapshot = queue.snapshot;
	const audio = attach(false);
	fetchFn.mockResolvedValueOnce(new Response(null, { status: 403 }));
	await player.setPrivateListening(true);
	expect(player.privateListening?.enabled).toBe(false);
	const started = Promise.withResolvers<void>(), reply = Promise.withResolvers<Response>();
	fetchFn.mockImplementationOnce(() => { started.resolve(); return reply.promise; });
	const playing = play(1);
	await started.promise;
	if (caseName === 'superseded') refresh();
	if (caseName === 'account retirement') lifetime.abort();
	const selected: number[] = [];
	onTestFinished(queue.subscribe((value) => selected.push(value.currentIndex)));
	const latest = caseName === 'newer play' ? play(0) : undefined;
	reply.resolve(grantReply(caseName, incumbent.split('/')[5]));
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
		expect(media.audio).toBe(audio);
		expect(audio.pause).not.toHaveBeenCalled();
	}
});

test.each(['wrong tab', 'server error', 'expired'] as const)('%s requires a fresh privacy acknowledgement before another start', async (reason) => {
	const { player, queue, fetchFn, src, play } = setup();
	const clock = vi.spyOn(Date, 'now');
	onTestFinished(() => clock.mockRestore());
	await play(0);
	const incumbent = src(), snapshot = queue.snapshot;
	if (reason === 'wrong tab') fetchFn.mockResolvedValueOnce(Response.json({ session_id: 'another-tab', private_until: Math.floor(Date.now() / 1000) + 60 }));
	if (reason === 'server error') fetchFn.mockResolvedValueOnce(new Response(null, { status: 503 }));
	await player.setPrivateListening(true);
	if (reason === 'expired') clock.mockReturnValue(Date.now() + 60_000);
	await play(1);
	expect(fetchFn).toHaveBeenCalledTimes(2);
	expect(queue.snapshot).toBe(snapshot);
	expect(src()).toBe(incumbent);
});
