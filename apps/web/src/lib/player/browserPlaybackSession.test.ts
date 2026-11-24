import { expect, test, vi } from 'vitest';
import { SESSION_BINDING_HEADER, type AccountSession } from '../auth-fence.js';
import { createPlayerController } from './controller.svelte.js';
import { createQueue, currentTrack } from './queue.js';

const binding = 'A'.repeat(43);
const initial = createQueue(['first', 'second'].map((id) => ({ id, title: id, artist: '', streamUrl: `/fixture/${id}` })));
function setup() {
	const lifetime = new AbortController();
	const account: AccountSession = { generation: 'first', signal: lifetime.signal,
		isCurrent: () => !lifetime.signal.aborted, run: async (work) => !lifetime.signal.aborted ? work() : undefined };
	let marked = false;
	const starts: boolean[] = [];
	const fetchFn = vi.fn<typeof fetch>(async (path, init) => {
		const { session_id, enabled, track_id, play_id } = JSON.parse(String(init?.body));
		expect(new Headers(init?.headers).get(SESSION_BINDING_HEADER)).toBe(binding);
		expect(init).toMatchObject({ credentials: 'include', cache: 'no-store', redirect: 'error' });
		if (path === '/v1/me/private-session') {
			expect(init?.method).toBe('PUT');
			marked = enabled;
			return Response.json({ session_id, private_until: marked ? Math.floor(Date.now() / 1000) + 60 : null });
		}
		expect([path, init?.method]).toEqual(['/v1/plays', 'POST']);
		expect(new Headers(init?.headers).get('x-nntune-browser-playback')).toBe('1');
		expect(play_id).toBeUndefined();
		starts.push(marked);
		return Response.json({ sessionBinding: binding, playbackSessionId: session_id, trackId: track_id,
			playId: `play-${starts.length}`, durationMs: 120_000 });
	});
	const player = createPlayerController(initial, { flush: async () => {} }, (node, source) => node.getAttribute('src') === source, { fetchFn });
	const { media, queue } = player;
	const refresh = () => {
		player.bindBrowserPlayback(account, binding);
		return player.reconcileLibrary(initial.tracks, () => true);
	};
	refresh();
	const src = () => player.streamFor(currentTrack(queue.snapshot));
	const play = (index: number) => media.playSelected(queue.snapshot, index, queue.sourceForQueuedPlay(initial.tracks[index].id)!);
	return { player, media, queue, fetchFn, starts, lifetime, refresh, src, play };
}

test('new plays use the acknowledged mode in the same tab and continuations keep the established source', async () => {
	const { player, media, queue, fetchFn, starts, refresh, src, play } = setup();
	try {
		expect(player.privateListening?.enabled).toBe(false);
		expect(src()).toBe('');
		expect(fetchFn).not.toHaveBeenCalled();
		await media.playCurrent();
		const first = src(), tab = first.split('/')[5];
		expect(first).toBe(`/v1/plays/media/first/${tab}/play-1`);
		const audio = Object.assign(new EventTarget(), { paused: true, ended: false, error: null, isConnected: true,
			getAttribute: () => first, play: vi.fn(async () => { audio.paused = false; }), pause: vi.fn() });
		media.attach(audio as unknown as HTMLAudioElement);
		await player.setPrivateListening(true);
		expect(player.privateListening?.enabled).toBe(true);
		refresh();
		await media.playCurrent();
		expect(audio.play).toHaveBeenCalledOnce();
		expect(src()).toBe(first);
		expect(fetchFn).toHaveBeenCalledTimes(2);
		await play(1);
		expect(src()).toBe(`/v1/plays/media/second/${tab}/play-2`);
		expect(queue.snapshot.tracks).toEqual(initial.tracks);
		await player.setPrivateListening(false);
		await play(0);
		expect(src()).toBe(`/v1/plays/media/first/${tab}/play-3`);
		expect(starts).toEqual([false, true, false]);
		expect(fetchFn.mock.calls.map(([, init]) => JSON.parse(String(init?.body)).session_id)).toEqual(Array(5).fill(tab));
	} finally { player.destroy(); }
});

test.each(['refused', 'superseded', 'account retirement', 'legacy reply', 'extra field'] as const)('%s fences grant publication', async (caseName) => {
	const { player, media, queue, fetchFn, lifetime, src, play, refresh } = setup();
	try {
		await play(0);
		const incumbent = src(), snapshot = queue.snapshot;
		const audio = Object.assign(new EventTarget(), { paused: false, ended: false, error: null, pause: vi.fn() });
		media.attach(audio as unknown as HTMLAudioElement);
		fetchFn.mockResolvedValueOnce(new Response(null, { status: 403 }));
		await player.setPrivateListening(true);
		expect(player.privateListening?.enabled).toBe(false);
		const started = Promise.withResolvers<void>(), reply = Promise.withResolvers<Response>();
		fetchFn.mockImplementationOnce(() => { started.resolve(); return reply.promise; });
		const playing = play(1);
		await started.promise;
		if (caseName === 'superseded') refresh();
		if (caseName === 'account retirement') lifetime.abort();
		const sessionId = incumbent.split('/')[5];
		const receipt = { sessionBinding: binding, playbackSessionId: sessionId, trackId: 'second', playId: 'late', durationMs: 120_000 };
		if (caseName === 'refused') reply.resolve(new Response(null, { status: 409 }));
		else if (caseName === 'legacy reply') reply.resolve(Response.json({ play_id: 'late', grant: 'synthetic' }));
		else reply.resolve(Response.json(caseName === 'extra field' ? { ...receipt, grant: 'synthetic' } : receipt));
		await playing;
		if (caseName === 'account retirement') {
			expect(player.privateListening).toBeUndefined();
			expect(queue.snapshot.tracks).toHaveLength(0);
			expect(audio.pause).toHaveBeenCalledOnce();
		} else {
			expect(queue.snapshot).toBe(snapshot);
			expect(src()).toBe(incumbent);
			expect(media.audio).toBe(audio);
			expect(audio.pause).not.toHaveBeenCalled();
		}
	} finally { player.destroy(); }
});

test.each(['wrong tab', 'server error', 'expired'] as const)('%s requires a fresh privacy acknowledgement before another start', async (reason) => {
	const { player, queue, fetchFn, src, play } = setup();
	const clock = vi.spyOn(Date, 'now');
	try {
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
	} finally { clock.mockRestore(); player.destroy(); }
});
