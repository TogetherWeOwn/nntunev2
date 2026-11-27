import { expect, onTestFinished, test, vi } from 'vitest';
import { MEDIA_BYTES, trackSalt } from '#lib/player/demoMedia.js';
import { binding, grantReply, setup, setupToggleCase } from '#lib/player/fixtures/browserPlayback.js';
import { fixtureLink } from '#lib/player/fixtures/music-video.js';
import { createMediaController, type MediaSelection } from '#lib/player/mediaController.js';
import { verifiedVideoId } from '#lib/player/musicVideo.js';
import { createQueue } from '#lib/player/queue.js';
import { createQueueController } from '#lib/player/queueController.js';
import { SESSION_BINDING_HEADER } from '#lib/auth-fence.js';
import appearanceInit from '../../../../static/appearance-init.js?raw';
import { GET as manifest } from '../../manifest.webmanifest/+server.js';
import { GET as artwork } from './artwork/+server.js';
import { GET as audio } from './audio/+server.js';

function requestFor(path: string, headers?: HeadersInit) {
	const url = new URL(path, 'https://example.test');
	return { url, request: new Request(url, { headers }) };
}

test.each(['', 'items=0-1', 'bytes=-', 'bytes=0-1,2-3', 'bytes=0-9007199254740992',
	'bytes=-9007199254740992', 'bytes=9007199254740992-', 'bytes=0-1.5'])(
	'refuses invalid Range %j without returning a WAV', async (range) => {
		const response = await audio(requestFor('/api/demo/audio', { range }) as Parameters<typeof audio>[0]);
		expect(response.status).toBe(416);
		expect(response.headers.get('content-range')).toBe(`bytes */${MEDIA_BYTES}`);
		expect(response.headers.get('cache-control')).toBe('no-store');
		expect(response.body).toBeNull();
	}
);

test('refuses ambiguous track selection and preserves full delivery with an unmatched If-Range', async () => {
	const duplicate = requestFor('/api/demo/audio?track=first-light&track=second-signal', { range: 'bytes=0-0' });
	const rejected = await audio(duplicate as Parameters<typeof audio>[0]);
	expect(rejected.status).toBe(400);
	expect(rejected.headers.get('cache-control')).toBe('no-store');
	expect(rejected.body).toBeNull();

	const conditional = requestFor('/api/demo/audio', { range: 'bytes=0-0', 'if-range': '"other"' });
	const full = await audio(conditional as Parameters<typeof audio>[0]);
	expect(full.status).toBe(200);
	expect(full.headers.get('content-range')).toBeNull();
	expect(full.headers.get('x-content-type-options')).toBe('nosniff');
	expect((await full.arrayBuffer()).byteLength).toBe(MEDIA_BYTES);
	expect(trackSalt({ toString: () => 'second-signal' } as unknown as string)).toBe(0);
});

test('pins the install manifest and confines direct SVG artwork responses', async () => {
	const install = await manifest(requestFor('/manifest.webmanifest?id=other') as Parameters<typeof manifest>[0]);
	expect(install.headers.get('content-type')).toBe('application/manifest+json');
	expect(install.headers.get('cache-control')).toBe('public, max-age=3600');
	expect(install.headers.get('x-content-type-options')).toBe('nosniff');
	expect(await install.json()).toMatchObject({ id: '/', start_url: '/', scope: '/', icons: [
		{ src: '/icons/icon-192.png', sizes: '192x192', type: 'image/png' },
		{ src: '/icons/icon-512.png', sizes: '512x512', type: 'image/png' }
	] });
	const image = await artwork(requestFor('/api/demo/artwork') as Parameters<typeof artwork>[0]);
	expect(image.headers.get('content-security-policy')).toBe("default-src 'none'; sandbox");
	expect(image.headers.get('x-content-type-options')).toBe('nosniff');
});

test('applies only exact stored appearance values and tolerates blocked storage', () => {
	const initialize = new Function('localStorage', 'document', appearanceInit);
	const setAttribute = vi.fn(), setItem = vi.fn(), getItem = vi.fn();
	const storage = { getItem, setItem }, document = { documentElement: { setAttribute } };
	for (const appearance of [null, ' auto ', 'DARK', 'dark ', 1]) {
		getItem.mockReturnValue(appearance);
		initialize(storage, document);
	}
	getItem.mockImplementationOnce(() => { throw new Error('Storage blocked'); });
	initialize(storage, document);
	expect(setAttribute).not.toHaveBeenCalled();
	for (const appearance of ['light', 'dark']) {
		getItem.mockReturnValue(appearance);
		initialize(storage, document);
		expect(setAttribute).toHaveBeenLastCalledWith('data-appearance', appearance);
	}
	expect(setItem).not.toHaveBeenCalled();
});

test('fixtures reject unsupported scenarios and malformed privacy writes', async () => {
	expect(() => grantReply('unknown' as never, 'tab')).toThrow('Unknown grant fixture case');
	await expect(setupToggleCase('unknown' as never)).rejects.toThrow('Unknown privacy fixture case');
	const { fetchFn, play, starts } = setup();
	await expect(fetchFn('/v1/me/private-session', {
		method: 'PUT', credentials: 'include', cache: 'no-store', redirect: 'error',
		headers: { [SESSION_BINDING_HEADER]: binding, 'content-type': 'application/json' },
		body: JSON.stringify({ session_id: crypto.randomUUID(), enabled: 'false' })
	})).rejects.toThrow('Expected a boolean privacy toggle');
	await play(0);
	expect(starts).toEqual([false]);
	expect(Object.isFrozen(fixtureLink)).toBe(true);
	expect(verifiedVideoId(fixtureLink, fixtureLink.recordingId)).toBe(fixtureLink.videoId);
	expect(verifiedVideoId(fixtureLink, 'unknown')).toBeNull();
});

function controller(prepare?: MediaSelection['prepare'], matchesNative?: Parameters<typeof createMediaController>[2]) {
	const base = createQueue([{ id: 'first', title: 'First', artist: '', streamUrl: '/first' }]);
	const commit = vi.fn(() => true), announce = vi.fn();
	const player = createMediaController(createQueueController(base), {
		commit, announce, prepare, flush: async () => {}, browseCopy: { invalidate() {} }
	}, matchesNative);
	onTestFinished(() => player.destroy());
	const source = { streamUrl: '/first', isCurrent: () => true };
	return { player, base, commit, announce, source };
}

test('a mismatched grant cannot commit a selection', async () => {
	const grant = { trackId: 'second', playId: 'play', streamUrl: '/second', isCurrent: () => true };
	const { player, base, commit, source } = controller(async (_id, _current, publish) => { publish(grant); return true; });
	await player.playSelected(base, 0, source);
	await player.playSelected(base, 0, grant, '', grant);
	expect(commit).not.toHaveBeenCalled();
});

test('queued play verifies the source before native play and after settlement', async () => {
	let nativeSource = '/other';
	const { player, base, announce, source } = controller(undefined, (_node, url) => nativeSource === url);
	const element = Object.assign(new EventTarget(), {
		isConnected: true, paused: true, ended: false, error: null, getAttribute: () => '/first',
		play: vi.fn(async () => { element.paused = false; nativeSource = '/other'; }),
		pause: () => { element.paused = true; }
	});
	player.attach(element as unknown as HTMLAudioElement);
	await player.playSelected(base, 0, source);
	expect(element.play).not.toHaveBeenCalled();
	nativeSource = '/first';
	await player.playSelected(base, 0, source);
	expect(element.play).toHaveBeenCalledOnce();
	expect(announce).not.toHaveBeenCalled();
});

test('invalid seek inputs leave a valid pending share intact', () => {
	const { player } = controller();
	const intent = player.beginShareInput();
	const routeIsCurrent = vi.fn(() => true);
	player.captureShareSeek(intent, { trackId: 'first', seconds: 12 }, routeIsCurrent);
	expect(player.captureShareSeek(intent, { trackId: 'first', seconds: NaN }, routeIsCurrent)).toBeUndefined();
	player.jump('forward', -1);
	expect(routeIsCurrent).toHaveBeenCalledOnce();
	expect(player.pendingShareSeek).toMatchObject({ seconds: 12 });
});
