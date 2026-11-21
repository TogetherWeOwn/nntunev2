import { expect, test, vi } from 'vitest';
import { get } from 'svelte/store';
import { createMediaController as createMedia } from './mediaController.js';
import { createQueue, type QueueState } from './queue.js';
import { createQueueController } from './queueController.js';

const createMediaController = (matchesNative?: Parameters<typeof createMedia>[2], initial: QueueState = createQueue([])) =>
	createMedia(createQueueController(initial), undefined, matchesNative);

test.each([true, false])('streamless play publishes one keyed outcome only for a current source (%s)', async (current) => {
	const queue = { tracks: [{ id: 'coral', title: 'Coral', artist: '', streamUrl: '' }], currentIndex: 0 };
	const announce = vi.fn();
	const flush = vi.fn(async () => {});
	let valid = true;
	const media = createMedia(createQueueController(queue), {
		commit: () => { valid = current; return true; }, flush, browseCopy: { invalidate: () => {} }, announce
	});
	await media.playSelected(queue, 0, { streamUrl: '', isCurrent: () => valid });
	if (current) expect(announce).toHaveBeenCalledExactlyOnceWith('Selected Coral; no stream to play.', true);
	else expect(announce).not.toHaveBeenCalled();
	expect(flush).not.toHaveBeenCalled();
});

const coral = createQueue([{ id: 'coral', title: 'Coral', artist: '', streamUrl: '/coral' }]);

class SeekableAudio extends EventTarget {
	readonly isConnected = true; readonly paused = true;
	getAttribute() { return '/coral'; }
	get duration() { return 120; }
	get seekable() { return { length: 1, start: () => 0, end: () => 120 }; }
	position = 30;
	get currentTime() { return this.position; }
	set currentTime(value: number) { this.position = value; }
}

class TransportAudio extends EventTarget {
	isConnected = true;
	paused = true;
	ended = false;
	error: { code: number } | null = null;
	currentTime = 30;
	constructor(readonly source = '/coral') { super(); }
	getAttribute() { return this.source; }
	play = vi.fn(async () => { this.paused = false; });
	pause = vi.fn(() => { this.paused = true; });
	load = vi.fn();
	get element() { return this as unknown as HTMLAudioElement; }
	deferPlay() {
		const attempt = Promise.withResolvers<void>();
		this.play.mockImplementationOnce(() => { this.paused = false; return attempt.promise; });
		return attempt;
	}
	emit(type: string) { this.dispatchEvent(new Event(type)); }
}

function compactTransport() {
	const queue = createQueueController(coral);
	const announce = vi.fn();
	const media = createMedia(queue, {
		commit: (next) => { queue.publish(next, queue.begin(next, 'replace')); return queue.snapshot === next; },
		flush: async () => {}, browseCopy: { invalidate: () => {} }, announce
	}, (audio, source) => audio.getAttribute('src') === source);
	const audio = new TransportAudio();
	const detach = media.attach(audio.element);
	return { audio, queue, media, detach, announce, status: () => get(media.playbackStatus) };
}

const coralSource = { streamUrl: '/coral', isCurrent: () => true };
test.each(['queue', 'shared'])('%s Play reports a resource error once with queue counts', async (path) => {
	const { audio, media, announce, status } = compactTransport();
	const pending = audio.deferPlay(), compact = path === 'shared' ? media.playCurrent() : undefined;
	const queued = media.playSelected(coral, 0, coralSource, 'Added 0, 1 already queued.');
	await Promise.resolve();
	audio.error = { code: path === 'shared' ? 2 : 4 };
	audio.emit('error');
	if (path === 'queue') pending.reject(new DOMException('Bad resource', 'NotSupportedError'));
	const message = 'Selected Coral; audio unavailable. Added 0, 1 already queued.';
	await vi.waitFor(() => expect(announce).toHaveBeenCalledExactlyOnceWith(message, true));
	if (path === 'shared') pending.reject(new DOMException('Late failure', 'AbortError'));
	await Promise.all([compact, queued]);
	expect(status()).toBe('error');
	expect(announce).toHaveBeenCalledExactlyOnceWith(message, true);
	await media.playCurrent();
	expect(audio.play).toHaveBeenCalledOnce();
	audio.paused = true;
	audio.error = null;
	audio.emit('emptied');
	expect(status()).toBe('paused');
	audio.paused = false;
	audio.emit('play');
	expect(status()).toBe('starting');
	audio.emit('playing');
	expect(status()).toBe('playing');
	audio.ended = true;
	audio.emit('ended');
	expect(status()).toBe('paused');
});

test.each(['playing', 'blocked', 'paused', 'resumed'])('same-track queue Play shares a pending compact outcome: %s', async (outcome) => {
	const { audio, media, announce, status } = compactTransport();
	const pending = audio.deferPlay(), compact = media.playCurrent();
	const queued = media.playSelected(coral, 0, coralSource);
	await Promise.resolve();
	expect(audio.play).toHaveBeenCalledOnce();
	if (outcome === 'paused') audio.pause(); // Do not dispatch the queued pause event before settlement.
	if (outcome === 'playing' || outcome === 'paused') pending.resolve();
	else if (outcome === 'blocked') { audio.paused = true; pending.reject(new DOMException('Blocked', 'NotAllowedError')); }
	else {
		audio.pause();
		await audio.play();
		audio.emit('pause'); // The queued pause event sees the resumed decoder.
		pending.reject(new DOMException('Interrupted by pause', 'AbortError'));
	}
	await Promise.all([compact, queued]);
	if (outcome === 'resumed' || outcome === 'paused') {
		if (outcome === 'resumed') audio.emit('playing');
		expect(status()).toBe(outcome === 'resumed' ? 'playing' : 'paused');
		expect(announce).not.toHaveBeenCalled();
		return;
	}
	expect(status()).toBe(outcome);
	expect(announce).toHaveBeenCalledExactlyOnceWith(outcome === 'playing'
		? 'Playing Coral.' : 'Selected Coral, paused: playback was blocked.', true);
	if (outcome === 'blocked') {
		await media.playCurrent();
		expect(status()).toBe('playing');
		expect(audio.load).not.toHaveBeenCalled();
		expect(audio.currentTime).toBe(30);
	}
});

test('compact Play starts in the gesture, deduplicates pending Play, and reads native settlement', async () => {
	const { audio, media, status } = compactTransport();
	expect(status()).toBe('paused');
	const pending = audio.deferPlay(), attempt = media.playCurrent();
	audio.emit('play');
	await media.playCurrent();
	expect(audio.play).toHaveBeenCalledOnce();
	expect(status()).toBe('starting');
	pending.resolve();
	await attempt;
	expect(status()).toBe('playing');
	media.pauseCurrent();
	expect(audio.pause).toHaveBeenCalledOnce();
	expect(status()).toBe('paused');
	expect(media.audio).toBe(audio.element);
	expect(audio.currentTime).toBe(30);
	expect(audio.load).not.toHaveBeenCalled();
});

test.each(['compact', 'native'])('%s Pause fences pending success, including after a newer start', async (path) => {
	const { audio, media, status } = compactTransport();
	const stale = audio.deferPlay(), first = media.playCurrent();
	if (path === 'compact') media.pauseCurrent();
	else { audio.pause(); audio.emit('pause'); }
	expect(status()).toBe('paused');
	const latest = audio.deferPlay(), second = media.playCurrent();
	stale.resolve();
	audio.emit('playing');
	await first;
	expect(status()).toBe('starting');
	latest.resolve();
	await second;
	expect(status()).toBe('playing');
});

test.each(['replacement', 'detach', 'account purge', 'destroy'])('late Play is inert after %s', async (boundary) => {
	const { audio, queue, media, detach, status } = compactTransport();
	const pending = audio.deferPlay(), attempt = media.playCurrent();
	let replacement: TransportAudio | undefined;
	if (boundary === 'replacement') {
		replacement = new TransportAudio('/kelp');
		const next = createQueue([{ ...coral.tracks[0]!, streamUrl: replacement.source }]);
		queue.publish(next, queue.begin(next, 'replace'));
		media.attach(replacement.element);
		detach();
	} else if (boundary === 'detach') detach();
	else if (boundary === 'account purge') queue.purgeAtAuthBoundary(() => media.purgeAtAuthBoundary(() => {}));
	else media.destroy();
	audio.emit('playing');
	if (boundary === 'replacement') pending.resolve();
	else pending.reject(new DOMException('Interrupted', 'AbortError'));
	await attempt;
	expect(status()).toBe(replacement ? 'paused' : 'unavailable');
	expect(media.audio).toBe(replacement?.element);
	expect(replacement?.play.mock.calls ?? []).toEqual([]);
	expect(audio.pause).toHaveBeenCalledOnce();
});

test('share capture refuses a different selection or a blank stream', () => {
	const queue = createQueueController(coral);
	const media = createMedia(queue);
	const intent = media.beginShareInput();
	const routeIsCurrent = () => true;
	expect(media.captureShareSeek(intent, { trackId: 'kelp', seconds: 45 }, routeIsCurrent)).toBeUndefined();
	const noStream = createQueue([{ ...coral.tracks[0]!, streamUrl: '  ' }]);
	queue.publish(noStream, queue.begin(noStream, 'replace'));
	expect(media.captureShareSeek(intent, { trackId: 'coral', seconds: 45 }, routeIsCurrent)).toBeUndefined();
	expect(media.pendingShareSeek).toBeNull();
});

test.each([
	['same ID and source', 'coral', '/coral', true],
	['same ID, different source', 'coral', '/new-coral', false],
	['different ID, same source', 'kelp', '/coral', false]
] as const)('native share retry rechecks the selection after a queue edit: %s', (_case, id, streamUrl, writes) => {
	const queue = createQueueController(coral);
	const media = createMedia(queue, undefined, (_audio, source) => source === '/coral');
	const native = new SeekableAudio();
	const seek = vi.spyOn(native, 'currentTime', 'set');
	media.attach(native as unknown as HTMLAudioElement);
	const target = { trackId: 'coral', seconds: 45 };
	media.captureShareSeek(media.beginShareInput(), target, () => true);
	const pending = media.pendingShareSeek;
	expect(pending).toMatchObject({ ...target, streamUrl: '/coral' });

	// Let metadata start on the original node; change the selection during its read.
	vi.spyOn(native, 'duration', 'get').mockImplementationOnce(() => {
		const next = createQueue([{ id, streamUrl, title: 'Updated metadata', artist: '' }]);
		queue.publish(next, queue.begin(next, 'replace'));
		return 120;
	});
	native.dispatchEvent(new Event('loadedmetadata'));
	if (writes) expect(seek).toHaveBeenCalledExactlyOnceWith(45);
	else expect(seek).not.toHaveBeenCalled();
	expect(media.pendingShareSeek).toBe(writes ? null : pending);
});

test('a stale share tick and an in-flight native event cannot apply a newer receipt', () => {
	const media = createMediaController((_audio, source) => source === '/coral', coral);
	const native = new SeekableAudio();
	const seek = vi.spyOn(native, 'currentTime', 'set');
	media.attach(native as unknown as HTMLAudioElement);
	const capture = (seconds: number) => media.captureShareSeek(media.beginShareInput(), { trackId: 'coral', seconds }, () => true);
	const staleTick = capture(45);
	expect(staleTick).toBeDefined();
	vi.spyOn(native, 'getAttribute').mockImplementationOnce(() => { capture(60); return '/coral'; });
	native.dispatchEvent(new Event('loadedmetadata'));
	staleTick?.();
	expect(seek).not.toHaveBeenCalled();
	expect(media.pendingShareSeek).toMatchObject({ seconds: 60, streamUrl: '/coral' });

	native.dispatchEvent(new Event('durationchange'));
	expect(seek).toHaveBeenCalledExactlyOnceWith(60);
	expect(media.pendingShareSeek).toBeNull();
});

test.each(['availability', 'position', 'assignment', 'readback'] as const)(
	'restart suppresses stale work after a replacement attachment during %s', (phase) => {
		const media = createMediaController((_audio, source) => source === '/coral', coral);
		const replacement = new SeekableAudio() as unknown as HTMLAudioElement;
		let position = 30;
		let reads = 0, armed = false;
		const native = new class extends SeekableAudio {
			play = vi.fn(); pause = vi.fn(); load = vi.fn();
			get duration() { if (armed && phase === 'availability') media.attach(replacement); return 120; }
			get currentTime() {
				if (armed && ++reads === (phase === 'position' ? 2 : phase === 'readback' ? 3 : 0)) media.attach(replacement);
				return position;
			}
			set currentTime(value: number) {
				position = value;
				if (armed && phase === 'assignment') media.attach(replacement);
			}
		}();
		const audio = native as unknown as HTMLAudioElement;
		const staleCleanup = media.attach(audio);
		media.jump('back');
		const jumpStatus = 'Back 10 seconds, now at 0:20.';
		expect(get(media.seekFeedback).jumpStatus).toBe(jumpStatus);
		armed = true;
		media.restart();
		staleCleanup();
		expect(media.audio).toBe(replacement);
		expect(position).toBe(phase === 'assignment' || phase === 'readback' ? 0 : 20);
		expect(get(media.seekFeedback)).toMatchObject({ restartStatusVersion: 0, jumpStatus: phase === 'readback' ? '' : jumpStatus });
		for (const method of [native.play, native.pause, native.load]) expect(method).not.toHaveBeenCalled();
	}
);

test('only the newer restart on an unchanged receipt can seek and publish', () => {
	const media = createMediaController(() => true, coral);
	const native = new SeekableAudio();
	const seek = vi.spyOn(native, 'currentTime', 'set');
	media.attach(native as unknown as HTMLAudioElement);
	const receipt = media.attachment;
	vi.spyOn(native, 'duration', 'get').mockImplementationOnce(() => { media.restart(); return NaN; });
	media.restart();
	expect(media.attachment).toBe(receipt);
	expect(seek).toHaveBeenCalledExactlyOnceWith(0);
	expect(get(media.seekFeedback)).toMatchObject({ restartStatus: 'Restarted Coral.', restartStatusVersion: 1 });
});

test('retiring a receipt aborts its listener bank without aborting the replacement', () => {
	const media = createMediaController();
	const attach = () => {
		const target = Object.assign(new EventTarget(), { paused: true, pause: vi.fn() });
		const listen = vi.spyOn(target, 'addEventListener');
		const cleanup = media.attach(target as unknown as HTMLAudioElement);
		const signal = (listen.mock.calls[0]?.[2] as AddEventListenerOptions | undefined)?.signal;
		expect(signal?.aborted).toBe(false);
		for (const [, , options] of listen.mock.calls) expect(options).toMatchObject({ signal });
		return { cleanup, signal, target };
	};

	const first = attach();
	const replacement = attach();
	expect(first.signal?.aborted).toBe(true);
	first.cleanup();
	expect(replacement.signal?.aborted).toBe(false);
	replacement.cleanup();
	replacement.cleanup();
	expect(replacement.signal?.aborted).toBe(true);
	expect(media.audio).toBeUndefined();

	const reset = attach();
	reset.target.paused = false;
	reset.target.pause.mockImplementationOnce(() => expect(reset.signal?.aborted).toBe(true));
	media.reset();
	expect(reset.target.pause).toHaveBeenCalledOnce();
	expect(reset.signal?.aborted).toBe(true);
	const destroyed = attach();
	media.destroy();
	expect(destroyed.signal?.aborted).toBe(true);
});

test('an observed mount cannot be reused, even by a new controller', () => {
	const media = createMediaController();
	const audio = Object.assign(new EventTarget(), { paused: true }) as unknown as HTMLAudioElement;
	media.attach(audio);
	media.destroy();
	const replacement = createMediaController();
	expect(() => replacement.attach(audio)).toThrow(/fresh audio element/);
	expect(replacement.attachment).toBeUndefined();
});

test('destroy retires the active node and refuses later mounts', () => {
	const media = createMediaController();
	const audio = Object.assign(new EventTarget(), { paused: false, pause: vi.fn() }) as unknown as HTMLAudioElement;
	const cleanup = media.attach(audio);
	media.destroy();
	media.attach(audio);
	cleanup();
	expect(media.audio).toBeUndefined();
	expect(audio.pause).toHaveBeenCalledOnce();
});
