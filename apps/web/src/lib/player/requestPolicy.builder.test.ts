import { afterEach, expect, it, vi } from 'vitest';
import { createPlayerSession } from './session.js';
import { createQueue, restoreQueue, serializeQueue, type Track } from './queue.js';
import { resolveTrackLinearGain } from './replayGain.js';

const tracks: Track[] = ['a', 'b'].map((id) => ({
	id, title: id, artist: '', streamUrl: `https://example.test/media/${id}`
}));

class NativeMedia extends EventTarget {
	src = tracks[0].streamUrl;
	paused = true;
	ended = false;
	error = null;
	currentTime = 0;
	duration = 60;
	volume = 1;
	muted = false;
	playbackRate = 1;
	readyState = 4;
	play = vi.fn(async () => { this.paused = false; this.emit('play'); this.emit('playing'); });
	pause = vi.fn(() => { this.paused = true; this.emit('pause'); });
	load = vi.fn();
	getAttribute(name: string) { return name === 'src' ? this.src : null; }
	emit(name: string) { this.dispatchEvent(new Event(name)); }
	get element() { return this as unknown as HTMLAudioElement; }
}

afterEach(() => vi.unstubAllGlobals());

for (const boundary of ['pause', 'clear', 'auth', 'dispose'] as const) {
	it(`restored valid upcoming URLs perform no resource work across ${boundary} and stale completion`, async () => {
		const request = vi.fn(() => { throw new Error('Unexpected speculative request'); });
		const construct = vi.fn(function () { throw new Error('Unexpected media or graph'); });
		vi.stubGlobal('fetch', request);
		vi.stubGlobal('Audio', construct);
		vi.stubGlobal('AudioContext', construct);
		vi.stubGlobal('webkitAudioContext', construct);
		const player = createPlayerSession({ flush: async () => {} });
		player.start(null);
		const restored = restoreQueue(serializeQueue(createQueue(tracks)), tracks);
		player.setQueue(restored);
		expect(player.snapshot().queue.tracks).toEqual(tracks);
		expect(player.snapshot().audio).toBeNull();
		const audio = new NativeMedia();
		const detach = player.attach(audio.element);
		let release!: () => void;
		audio.play.mockImplementationOnce(() => new Promise<void>((resolve) => {
			audio.paused = false;
			audio.emit('play');
			release = resolve;
		}));
		const pending = player.play();
		await Promise.resolve();
		if (boundary === 'pause') player.pause();
		else if (boundary === 'clear') player.setQueue(createQueue([]));
		else if (boundary === 'auth') player.resetAtAuthBoundary();
		else player.destroy();
		release();
		await pending;
		detach();
		player.destroy();
		expect(request).not.toHaveBeenCalled();
		expect(construct).not.toHaveBeenCalled();
		expect(audio.load).not.toHaveBeenCalled();
		expect(audio.play).toHaveBeenCalledTimes(1);
		expect(audio.paused).toBe(true);
	});
}

for (const enabled of [false, true]) {
	it(`the pure absence input resolves exactly unity with preamp zero, enabled=${enabled}`, () => {
		// This is the pure helper's absence contract, not a claim that measured
		// production tags or a graph have been integrated into native playback.
		expect(resolveTrackLinearGain({}, { enabled, preampDb: 0 })).toBe(1);
	});
}
