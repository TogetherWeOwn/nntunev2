import { expect, onTestFinished, vi } from 'vitest';
import { SESSION_BINDING_HEADER, type AccountSession } from '../../auth-fence.js';
import { createPlayerController } from '../controller.svelte.js';
import { createQueue, currentTrack } from '../queue.js';

export const binding = 'A'.repeat(43);
export const initial = createQueue(['first', 'second'].map((id) => ({ id, title: id, artist: '', streamUrl: `/fixture/${id}` })));

export const receipt = (tab: string, track: string, play: string) => ({ sessionBinding: binding,
	playbackSessionId: tab, trackId: track, playId: play, durationMs: 120_000 });

export function grantReply(mode: string, tab: string) {
	if (mode === 'refused') return new Response(null, { status: 409 });
	if (mode === 'legacy reply') return Response.json({ play_id: 'late', grant: 'synthetic' });
	const current = receipt(tab, 'second', 'late');
	return Response.json(mode === 'extra field' ? { ...current, grant: 'synthetic' } : current);
}

export function setup(privateTtlSeconds = 60) {
	const lifetime = new AbortController();
	const account: AccountSession = { generation: 'first', signal: lifetime.signal,
		isCurrent: () => !lifetime.signal.aborted, run: async (work) => !lifetime.signal.aborted ? work() : undefined };
	let marked = false;
	const starts: boolean[] = [];
	const respond: typeof fetch = async (path, init) => {
		const { session_id, enabled, track_id, play_id } = JSON.parse(String(init?.body));
		expect(new Headers(init?.headers).get(SESSION_BINDING_HEADER)).toBe(binding);
		expect(init).toMatchObject({ credentials: 'include', cache: 'no-store', redirect: 'error' });
		if (path === '/v1/me/private-session') {
			expect(init?.method).toBe('PUT');
			marked = enabled;
			return Response.json({ session_id, private_until: marked ? Math.floor(Date.now() / 1000) + privateTtlSeconds : null });
		}
		expect([path, init?.method]).toEqual(['/v1/plays', 'POST']);
		expect(new Headers(init?.headers).get('x-nntune-browser-playback')).toBe('1');
		if (play_id) return Response.json(receipt(session_id, track_id, play_id));
		starts.push(marked);
		return Response.json(receipt(session_id, track_id, `play-${starts.length}`));
	};
	const fetchFn = vi.fn(respond);
	const player = createPlayerController(initial, { flush: async () => { if (src() && !media.audio) attach(); } },
		(node, source) => node.getAttribute('src') === source, { fetchFn });
	onTestFinished(() => player.destroy());
	const { media, queue } = player;
	const refresh = () => {
		player.bindBrowserPlayback(account, binding);
		return player.reconcileLibrary(initial.tracks, () => true);
	};
	refresh();
	const src = () => player.streamFor(currentTrack(queue.snapshot));
	const play = (index: number) => media.playSelected(queue.snapshot, index, queue.sourceForQueuedPlay(initial.tracks[index].id)!);
	function attach(paused = true) {
		const locator = src();
		const audio = Object.assign(new EventTarget(), { paused, ended: false, error: null as MediaError | null,
			isConnected: true, currentTime: 0, duration: 120, getAttribute: () => locator,
			play: vi.fn(async () => { audio.paused = false; }), pause: vi.fn(),
			load: vi.fn(() => { audio.error = null; audio.currentTime = 0; }),
			failAt(seconds: number) {
				audio.currentTime = seconds;
				audio.error = { code: 2 } as MediaError;
				audio.dispatchEvent(new Event('error'));
			},
			finish() {
				audio.dispatchEvent(new Event('play'));
				audio.ended = true;
				audio.dispatchEvent(new Event('ended'));
			} });
		media.attach(audio as unknown as HTMLAudioElement);
		return audio;
	}
	function delayGrant() {
		const started = Promise.withResolvers<void>(), reply = Promise.withResolvers<Response | undefined>();
		fetchFn.mockImplementationOnce(async (...args) => {
			started.resolve();
			return (await reply.promise) ?? respond(...args);
		});
		return { started: started.promise, reply: reply.resolve };
	}
	return { player, media, queue, fetchFn, starts, lifetime, refresh, src, play, attach, delayGrant };
}

export const refusedContinuations = (tab: string) => [
	new Response(null, { status: 409 }), Response.json(receipt(tab, 'first', 'other-play'))
];

export function expireInsideRequestWindow() {
	// Wall clock moves backwards and the advertised expiry is an hour beyond the server's TTL.
	// The monotonic clock is past the request margin but still inside the six-hour window.
	const wall = vi.spyOn(Date, 'now').mockReturnValue(Date.now() - 3_600_000);
	const monotonic = vi.spyOn(performance, 'now').mockReturnValue(performance.now() + 21_600_000 - 15_000);
	onTestFinished(() => { wall.mockRestore(); monotonic.mockRestore(); });
}
