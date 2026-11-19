import { expect, test, vi } from 'vitest';
import { createMediaController } from './mediaController.js';

test('feedback that retires the attempt cannot also bump its live region', async () => {
	const media = createMediaController();
	const queue = { tracks: [{ id: 'coral', title: 'Coral', artist: '', streamUrl: '' }], currentIndex: 0 };
	const announce = vi.fn(() => media.destroy());
	const bumpAnnouncement = vi.fn();
	const flush = vi.fn(async () => {});
	await media.playSelected(queue, 0, { streamUrl: '', isCurrent: () => true }, '', {
		revision: { revoke: () => 1, isCurrent: () => true },
		commit: () => true, flush, announce, bumpAnnouncement
	});
	expect(announce).toHaveBeenCalledExactlyOnceWith('Selected Coral; no stream to play.');
	expect(flush).not.toHaveBeenCalled();
	expect(bumpAnnouncement).not.toHaveBeenCalled();
});

test.each(['availability', 'position', 'assignment', 'readback'] as const)(
	'restart suppresses stale work after a same-node reattachment during %s', (phase) => {
		const media = createMediaController((_audio, source) => source === '/coral');
		let position = 30;
		const native = {
			isConnected: true, getAttribute: () => '/coral', play: vi.fn(), pause: vi.fn(), load: vi.fn(),
			get currentTime() {
				if (phase === (position === 0 ? 'readback' : 'position')) media.attach(audio);
				return position;
			},
			set currentTime(value: number) {
				position = value;
				if (phase === 'assignment') media.attach(audio);
			}
		};
		const audio = native as unknown as HTMLAudioElement;
		const staleCleanup = media.attach(audio);
		const assigned = vi.fn(), publish = vi.fn();
		media.restart('/coral', 'Coral', {
			supersedeShare: () => () => true,
			available: () => { if (phase === 'availability') media.attach(audio); return true; },
			assigned, publish
		});
		staleCleanup();
		expect(media.audio).toBe(audio);
		expect(position).toBe(phase === 'assignment' || phase === 'readback' ? 0 : 30);
		expect(assigned).toHaveBeenCalledTimes(phase === 'readback' ? 1 : 0);
		expect(publish).not.toHaveBeenCalled();
		for (const method of [native.play, native.pause, native.load]) expect(method).not.toHaveBeenCalled();
	}
);

test('only the newer restart on an unchanged receipt can seek and publish', () => {
	const media = createMediaController((_audio, source) => source === '/coral');
	const seek = vi.fn();
	const native = {
		isConnected: true, getAttribute: () => '/coral',
		get currentTime() { return seek.mock.calls.length ? 0 : 30; },
		set currentTime(value: number) { seek(value); }
	};
	media.attach(native as unknown as HTMLAudioElement);
	const receipt = media.attachment;
	const assigned = vi.fn(), publish = vi.fn();
	const deps = { supersedeShare: () => () => true, available: () => true, assigned, publish };
	media.restart('/coral', 'Older', {
		...deps, available: () => { media.restart('/coral', 'Newer', deps); return true; }
	});
	expect(media.attachment).toBe(receipt);
	expect(seek).toHaveBeenCalledExactlyOnceWith(0);
	expect(assigned).toHaveBeenCalledOnce();
	expect(publish).toHaveBeenCalledExactlyOnceWith('Restarted Newer.', true);
});

test('cleanup belongs to the attachment, even when the same node is attached twice', () => {
	const media = createMediaController();
	const pause = vi.fn(() => expect(media.audio).toBeUndefined());
	const audio = { paused: false, pause } as unknown as HTMLAudioElement;
	const stale = media.attach(audio);
	const cleanup = media.attach(audio);
	stale();
	expect(media.audio).toBe(audio);
	expect(pause).not.toHaveBeenCalled();
	cleanup();
	cleanup();
	expect(media.audio).toBeUndefined();
	expect(pause).toHaveBeenCalledOnce();
});

test('retiring a receipt aborts its listener bank without aborting the replacement', () => {
	const media = createMediaController();
	const observations = {
		source: () => null, interrupt: vi.fn(), ended: vi.fn(async () => {}),
		seeking: vi.fn(), metadata: vi.fn(), position: vi.fn()
	};
	const attach = () => {
		const target = Object.assign(new EventTarget(), { paused: true, pause: vi.fn() });
		const listen = vi.spyOn(target, 'addEventListener');
		const cleanup = media.attach(target as unknown as HTMLAudioElement, observations);
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
	expect(replacement.signal?.aborted).toBe(true);

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

test('a receipt-only mount cannot later be observed, even by a new controller', () => {
	const media = createMediaController();
	const audio = Object.assign(new EventTarget(), { paused: true }) as unknown as HTMLAudioElement;
	media.attach(audio);
	media.destroy();
	const replacement = createMediaController();
	expect(() => replacement.attach(audio, {
		source: () => null, interrupt: vi.fn(), ended: vi.fn(async () => {}),
		seeking: vi.fn(), metadata: vi.fn(), position: vi.fn()
	})).toThrow(/fresh audio element/);
	expect(replacement.attachment).toBeUndefined();
});

test('destroy retires the active node and refuses later mounts', () => {
	const media = createMediaController();
	const audio = { paused: false, pause: vi.fn() } as unknown as HTMLAudioElement;
	const cleanup = media.attach(audio);
	media.destroy();
	media.attach(audio);
	cleanup();
	expect(media.audio).toBeUndefined();
	expect(audio.pause).toHaveBeenCalledOnce();
});
