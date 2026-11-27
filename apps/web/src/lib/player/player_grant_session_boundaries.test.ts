import { afterEach, expect, it, onTestFinished, vi } from 'vitest';
import { writable } from 'svelte/store';
import type { AccountSession } from '../auth-fence.js';
import { LIBRARY_AUTH_CONTEXT_KEY, LIBRARY_QUEUE_KEY } from '../library/cache.js';
import { emptySeek } from './absoluteSeek.js';
import { createBrowserPlaybackGrant } from './browserPlaybackGrant.js';
import { createBrowserPlaybackSession } from './browserPlaybackSession.js';
import { createBufferScheduler } from './bufferScheduler.js';
import { createPlayerController } from './controller.svelte.js';
import { createLibraryQueuePersistence } from './libraryQueuePersistence.js';
import { createListenEventSender } from './listenEventSender.js';
import { createM3UDownload } from './m3uDownload.js';
import { createQueue } from './queue.js';
import { createResumeController } from './resumeController.js';
import { actionForShortcutEvent } from './shortcuts.js';
import { applySoundPreference, parseSoundPreference } from './soundPreference.js';
import { bindSystemSeek } from './systemSeek.js';
import { createDemoTabTitle } from './tabTitle.js';

const binding = 'A'.repeat(43);
const track = { id: 'track', title: 'Song', artist: 'Artist', streamUrl: '', durationMs: 1000 };
const always = () => true;

function account(): AccountSession {
	const lifetime = new AbortController();
	return { generation: 'synthetic', signal: lifetime.signal, isCurrent: always, run: async (work) => work() };
}

afterEach(() => {
	vi.restoreAllMocks();
	vi.unstubAllGlobals();
});

it('refuses a failed grant dispatch or inherited receipt and can open again', async () => {
	const owner = account(), publish = vi.fn();
	const prepare = vi.fn().mockImplementationOnce(() => { throw new Error('Offline'); })
		.mockImplementationOnce(async (target) => ({
			data: Object.assign(Object.create({ trackId: track.id }), {
				sessionBinding: binding, playbackSessionId: target.playbackSessionId,
				playId: 'issued', durationMs: 1000, extra: true
			})
		}))
		.mockImplementation(async (target) => ({ data: { ...target, playId: 'issued', durationMs: 1000 } }));
	const grant = createBrowserPlaybackGrant(owner, binding, prepare);
	const open = () => grant.open({ trackId: track.id }, owner.signal, always, publish);
	await expect(open()).resolves.toBe(false);
	await expect(open()).resolves.toBe(false);
	expect(publish).not.toHaveBeenCalled();
	await expect(open()).resolves.toBe(true);
});

it('binds retries to the published receipt and requires a complete privacy acknowledgement', async () => {
	const fetchFn = vi.fn<typeof fetch>().mockImplementation(async (_path, init) => {
		const body = JSON.parse(String(init?.body));
		if ('enabled' in body) return Response.json({ session_id: body.session_id, private_until: null, extra: true });
		return Response.json({
			sessionBinding: binding, playbackSessionId: body.session_id, trackId: body.track_id,
			playId: body.play_id ?? 'issued', durationMs: 1000
		});
	});
	const show = vi.fn(), publish = vi.fn();
	const session = createBrowserPlaybackSession(account(), binding, show, fetchFn);
	onTestFinished(session.dispose);
	expect(await session.open(track.id, always, publish, 'issued')).toBe(false);
	expect(fetchFn).not.toHaveBeenCalled();
	expect(await session.open(track.id, always, publish)).toBe(true);
	await session.setPrivate(false);
	expect(show).toHaveBeenLastCalledWith({ enabled: false, pending: false, feedback: 'Could not change private listening.' });
	expect(await session.open(track.id, always, publish)).toBe(false);
	expect(await session.open('other', always, publish, 'issued')).toBe(false);
	expect(await session.open(track.id, always, publish, 'issued')).toBe(true);
	expect(fetchFn).toHaveBeenCalledTimes(3);
});

it.each(['0', '901', 'invalid'])('refuses a grant with an invalid explicit TTL (%s)', async (ttl) => {
	const fetchFn: typeof fetch = async (_path, init) => Response.json({
		sessionBinding: binding, playbackSessionId: JSON.parse(String(init?.body)).session_id,
		trackId: track.id, playId: 'issued', durationMs: 1000
	}, { headers: { 'x-nntune-playback-ttl': ttl } });
	const publish = vi.fn();
	const session = createBrowserPlaybackSession(account(), binding, vi.fn(), fetchFn);
	onTestFinished(session.dispose);
	expect(await session.open(track.id, always, publish)).toBe(false);
	expect(publish).not.toHaveBeenCalled();
});

it('rechecks the queue storage scope before publishing a restore or saving', () => {
	const key = `${LIBRARY_QUEUE_KEY}.first`;
	const rows = new Map([[LIBRARY_AUTH_CONTEXT_KEY, 'first'], [key, '{"orderIds":["track"],"currentId":"track"}']]);
	const storage = { getItem: (id: string) => rows.get(id) ?? null, setItem: vi.fn(), removeItem: vi.fn() };
	const persistence = createLibraryQueuePersistence();
	expect(persistence.restore(storage, createQueue([]), () => {
		rows.delete(LIBRARY_AUTH_CONTEXT_KEY);
		return [track];
	})).toBeNull();
	persistence.save(createQueue([track]));
	expect(storage.setItem).not.toHaveBeenCalled();
	expect(storage.removeItem).not.toHaveBeenCalled();
});

it('keeps the latest Library source evidence across reentrant reconciliation', () => {
	const player = createPlayerController(createQueue([track]), { flush: async () => {} });
	onTestFinished(player.destroy);
	expect(player.reconcileLibrary([{ id: track.id, streamUrl: '/old' }], always, () => {
		expect(player.reconcileLibrary([{ id: track.id, streamUrl: '/current' }], always)).toBe(true);
	})).toBe(false);
	expect(player.queue.sourceForQueuedPlay(track.id)?.streamUrl).toBe('/current');
});

it.each(['context', 'start'] as const)('refuses a history %s response with extra fields', async (phase) => {
	vi.useFakeTimers();
	onTestFinished(() => { vi.useRealTimers(); });
	const fetchFn = vi.fn<typeof fetch>()
		.mockResolvedValueOnce(Response.json(phase === 'context' ? { context: binding, extra: true } : { context: binding }))
		.mockImplementation(async () => Response.json({ ok: true, extra: true }));
	const playId = 'QMMlwCzuSJ2otvvpdBVWAQ';
	const sender = createListenEventSender({
		owner: account(), canRecord: always, fetchFn,
		grantForStart: () => ({ trackId: track.id, playId, streamUrl: '', isCurrent: always })
	});
	onTestFinished(sender.dispose);
	await sender.ready;
	const event = { playback_id: '40c325c0-2cee-489d-a8b6-fbe974155601', track_id: track.id };
	sender.send({ ...event, event: 'start' });
	sender.send({ ...event, event: 'complete' });
	await vi.advanceTimersByTimeAsync(1);
	await sender.close();
	expect(fetchFn).toHaveBeenCalledTimes(phase === 'start' ? 2 : 1);
});

it('disconnects both buffer paths and preserves the startup error after rollback', () => {
	const first = { connect: vi.fn(), disconnect: vi.fn(), start: vi.fn(), stop: vi.fn(() => { throw new Error('Stop'); }) };
	const failure = new Error('Start');
	const second = { ...first, disconnect: vi.fn(), start: vi.fn(() => { throw failure; }) };
	const createBufferSource = vi.fn().mockReturnValueOnce(first).mockReturnValueOnce(second);
	const disconnect = vi.fn();
	const context = { currentTime: 0, createBufferSource, createGain: () => ({ connect: vi.fn(), disconnect }) };
	const scheduler = createBufferScheduler(context as unknown as Parameters<typeof createBufferScheduler>[0]);
	const buffer = { duration: 2 } as AudioBuffer;
	expect(() => scheduler.play(buffer, buffer)).toThrow(failure);
	expect(first.disconnect).toHaveBeenCalledOnce();
	expect(second.disconnect).toHaveBeenCalledOnce();
	expect(disconnect).toHaveBeenCalledTimes(2);
	expect(() => scheduler.stop()).not.toThrow();
});

it('revokes a download URL immediately when clicking fails', () => {
	vi.stubGlobal('document', { body: { append: vi.fn() } });
	vi.spyOn(URL, 'createObjectURL').mockReturnValue('blob:synthetic');
	const revoke = vi.spyOn(URL, 'revokeObjectURL');
	const link = { click: () => { throw new Error('Click'); } } as unknown as HTMLAnchorElement;
	const download = createM3UDownload();
	expect(() => download.download(link, () => '#EXTM3U', 'queue.m3u')).toThrow('Click');
	expect(revoke).toHaveBeenCalledExactlyOnceWith('blob:synthetic');
	download.release();
	expect(revoke).toHaveBeenCalledOnce();
});

it('preserves resume state if completion is lost while resolving storage', () => {
	const audio = { isConnected: true, src: '/track', currentSrc: '/track', getAttribute: () => '/track', ended: true, seeking: false, error: null };
	const storage = { getItem: vi.fn(() => '{"track":42}'), setItem: vi.fn(), removeItem: vi.fn() };
	const resume = createResumeController();
	resume.setScope('a'.repeat(64));
	const receipt = resume.attach({ trackId: track.id, audio: audio as unknown as HTMLAudioElement, source: '/track',
		ownsAttachment: always, storage: () => { audio.ended = false; return storage; } });
	receipt?.observe('ended');
	expect(storage.setItem).not.toHaveBeenCalled();
});

it('leaves keys with native editors inside a composed event path', () => {
	const event = { key: 'k', target: { tagName: 'X-EDITOR' }, composedPath: () => [{ tagName: 'INPUT' }] } as unknown as KeyboardEvent;
	expect(actionForShortcutEvent(event, { querySelector: () => null, querySelectorAll: () => [] })).toBeNull();
});

it('bounds stored sound preferences and refuses a malformed live preference', () => {
	expect(parseSoundPreference(' '.repeat(2048) + '{"version":1,"volume":0.25,"muted":true}')).toEqual({ volume: 1, muted: false });
	const element = { volume: 0.5, muted: false };
	expect(applySoundPreference(element, { volume: NaN, muted: true })).toEqual({ volume: 0.5, muted: false, restricted: true });
	expect(element).toEqual({ volume: 0.5, muted: false });
});

it('keeps seek handlers installed when a newer intent supersedes the decoder read', () => {
	let intent = 0;
	const audio = { get currentTime() { intent++; return NaN; }, duration: 20, error: null, seeking: false } as HTMLAudioElement;
	const availability = writable({ ...emptySeek, enabled: true, attachment: 1 });
	const dispatch = vi.fn();
	const lifetime = new AbortController();
	onTestFinished(() => lifetime.abort());
	const transport = bindSystemSeek({ metadata: null, setActionHandler: vi.fn() },
		{ audio, attachment: 1, availability, matches: always, positionIntent: () => intent, dispatch }, lifetime.signal);
	const handlers = transport.handlers;
	handlers?.onSeekTo?.({ action: 'seekto', seekTime: 4 });
	expect(handlers).toBeDefined();
	expect(transport.handlers).toBe(handlers);
	expect(dispatch).not.toHaveBeenCalled();
});

it('ignores title writes and teardown from a superseded controller', () => {
	const target = { title: 'Original' };
	const old = createDemoTabTitle(target);
	const current = createDemoTabTitle(target);
	current.update(track, true);
	old.update(null, false);
	old.restore();
	expect(target.title).toBe('Song — Artist');
	current.restore();
	expect(target.title).toBe('Original');
});
