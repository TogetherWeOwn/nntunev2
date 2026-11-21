import { afterEach, expect, test, vi } from 'vitest';
import { createMediaController } from './mediaController.js';
import type { MediaMetadataInit, MediaSessionLike, MediaSessionPosition } from './mediaSession.js';
import { createQueue } from './queue.js';
import { createQueueController } from './queueController.js';

const first = { id: 'first', title: 'First', artist: 'Fixture Artist', streamUrl: '/first',
	artworkUrl: 'https://art.example.test/first.png' };
const second = { ...first, id: 'second', title: 'Second', streamUrl: '/second' };

class Decoder extends EventTarget {
	readonly HAVE_METADATA = 1;
	readyState = 1;
	isConnected = true;
	paused = true;
	ended = false;
	error: { code: number } | null = null;
	duration = 120;
	position = 0;
	rate = 1;
	constructor(readonly source: string) { super(); }
	get playbackRate() { return this.rate; }
	set playbackRate(rate: number) { this.rate = rate; }
	get currentTime() { return this.position; }
	getAttribute() { return this.source; }
	emit(type: string) { this.dispatchEvent(new Event(type)); }
	get element() { return this as unknown as HTMLAudioElement; }
}

function setup(support: 'available' | 'missing' | 'throws' = 'available', ready = true) {
	const setPositionState = vi.fn<(state?: MediaSessionPosition) => void>();
	const session: MediaSessionLike = { metadata: null, setActionHandler: vi.fn() };
	if (support !== 'missing') session.setPositionState = setPositionState;
	if (support === 'throws') setPositionState.mockImplementation(() => { throw new Error('unsupported'); });
	vi.stubGlobal('navigator', { mediaSession: session });
	vi.stubGlobal('MediaMetadata', class {
		constructor(init: MediaMetadataInit) { Object.assign(this, init); }
	});
	const queue = createQueueController(createQueue([first]));
	const media = createMediaController(queue, undefined, () => true);
	const audio = new Decoder(first.streamUrl);
	if (!ready) audio.readyState = 0;
	const detach = media.attach(audio.element);
	return { setPositionState, session, queue, media, audio, detach };
}

afterEach(() => vi.unstubAllGlobals());

test('publishes observed decoder metadata, time, seek and rate without rebuilding transport', () => {
	const { audio, session, setPositionState, queue, media } = setup('available', false);
	expect(setPositionState).toHaveBeenLastCalledWith(undefined);
	const metadata = session.metadata as MediaMetadataInit;
	expect(metadata.artwork?.[0]?.src).not.toContain('art.example.test');
	vi.mocked(session.setActionHandler).mockClear();

	audio.readyState = 1;
	audio.emit('loadedmetadata');
	expect(setPositionState).toHaveBeenLastCalledWith({ duration: 120, position: 0, playbackRate: 1 });
	audio.position = 7;
	audio.emit('timeupdate');
	expect(setPositionState).toHaveBeenLastCalledWith({ duration: 120, position: 7, playbackRate: 1 });
	audio.position = 36;
	audio.emit('seeking');
	expect(setPositionState).toHaveBeenLastCalledWith({ duration: 120, position: 36, playbackRate: 1 });
	audio.position = 35.5; // The decoder may land somewhere other than the requested time.
	audio.emit('seeked');
	expect(setPositionState).toHaveBeenLastCalledWith({ duration: 120, position: 35.5, playbackRate: 1 });
	audio.playbackRate = 1.25;
	audio.emit('ratechange');
	expect(setPositionState).toHaveBeenLastCalledWith({ duration: 120, position: 35.5, playbackRate: 1.25 });
	audio.position = 120;
	audio.emit('pause');
	expect(setPositionState).toHaveBeenLastCalledWith({ duration: 120, position: 120, playbackRate: 1.25 });
	expect(session.metadata).toBe(metadata);
	expect(session.setActionHandler).not.toHaveBeenCalled();

	const previousState = setPositionState.mock.lastCall;
	const edited = createQueue([{ ...first, title: 'Updated' }]);
	queue.publish(edited, queue.begin(edited, 'replace'));
	expect(session.metadata).toMatchObject({ title: 'Updated' });
	expect(setPositionState.mock.calls.slice(-1)).toEqual([previousState]);
	media.destroy();
});

test.each([
	['duration', NaN, 'durationchange'], ['duration', Infinity, 'durationchange'],
	['duration', 0, 'durationchange'], ['duration', -1, 'durationchange'],
	['playbackRate', NaN, 'ratechange'], ['playbackRate', Infinity, 'ratechange'],
	['playbackRate', 0, 'ratechange'], ['playbackRate', -1, 'ratechange'],
	['position', NaN, 'timeupdate'], ['position', -1, 'timeupdate'], ['position', 121, 'timeupdate']
] as const)('clears instead of publishing invalid %s %s', (key, value, event) => {
	const { audio, setPositionState, media } = setup();
	expect(setPositionState).toHaveBeenLastCalledWith({ duration: 120, position: 0, playbackRate: 1 });
	audio[key] = value;
	audio.emit(event);
	expect(setPositionState).toHaveBeenLastCalledWith(undefined);
	media.destroy();
});

test('clears on decoder unload and terminal error even if numeric fields look usable', () => {
	const { audio, setPositionState, media } = setup();
	audio.readyState = 0;
	audio.emit('emptied');
	expect(setPositionState).toHaveBeenLastCalledWith(undefined);
	audio.readyState = 1;
	audio.emit('loadedmetadata');
	expect(setPositionState).toHaveBeenLastCalledWith({ duration: 120, position: 0, playbackRate: 1 });
	audio.error = { code: 4 };
	audio.emit('error');
	expect(setPositionState).toHaveBeenLastCalledWith(undefined);
	media.destroy();
});

test.each(['detach', 'destroy', 'auth purge'] as const)('clears on %s and ignores retired decoder events', (end) => {
	const { audio, setPositionState, media, queue, detach } = setup();
	if (end === 'detach') detach();
	else if (end === 'destroy') media.destroy();
	else queue.purgeAtAuthBoundary(() => media.purgeAtAuthBoundary(() => {
		expect(setPositionState).toHaveBeenLastCalledWith(undefined);
	}));
	expect(setPositionState).toHaveBeenLastCalledWith(undefined);
	setPositionState.mockClear();
	audio.position = 15;
	for (const type of ['timeupdate', 'ratechange', 'seeked', 'emptied']) audio.emit(type);
	expect(setPositionState).not.toHaveBeenCalled();
	media.destroy();
});

test('a source change clears old position and stale events cannot overwrite a successor', () => {
	const { audio, setPositionState, queue, media, detach } = setup();
	const next = createQueue([second]);
	queue.publish(next, queue.begin(next, 'replace'));
	expect(setPositionState).toHaveBeenLastCalledWith(undefined);
	setPositionState.mockClear();
	audio.emit('seeked'); // Old node is still attached but its source is no longer current.
	expect(setPositionState).not.toHaveBeenCalled();
	const newer = new Decoder(second.streamUrl);
	newer.duration = 20;
	media.attach(newer.element);
	expect(setPositionState).toHaveBeenLastCalledWith({ duration: 20, position: 0, playbackRate: 1 });
	setPositionState.mockClear();
	detach();
	for (const type of ['timeupdate', 'seeked', 'emptied']) audio.emit(type);
	expect(setPositionState).not.toHaveBeenCalled();
	media.destroy();
});

test('drops a decoder snapshot if the attachment changes while reading it', () => {
	const { audio, setPositionState, media } = setup();
	const newer = new Decoder(first.streamUrl);
	newer.duration = 60;
	const readRate = vi.spyOn(audio, 'playbackRate', 'get').mockImplementationOnce(() => {
		media.attach(newer.element);
		return 1.5;
	});
	audio.emit('timeupdate');
	expect(readRate).toHaveBeenCalledOnce();
	expect(setPositionState).toHaveBeenLastCalledWith({ duration: 60, position: 0, playbackRate: 1 });
	media.destroy();
});

test.each(['missing', 'throws'] as const)('a %s position API does not break metadata or teardown', (support) => {
	const { audio, session, media } = setup(support);
	audio.position = 15;
	audio.emit('timeupdate');
	expect(session.metadata).toMatchObject({ title: first.title });
	expect(session.setActionHandler).toHaveBeenCalledWith('play', expect.any(Function));
	media.destroy();
	expect(session.metadata).toBeNull();
	expect(session.setActionHandler).toHaveBeenCalledWith('play', null);
});
