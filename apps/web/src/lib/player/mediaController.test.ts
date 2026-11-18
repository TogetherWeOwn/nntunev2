import { expect, test, vi } from 'vitest';
import { createMediaController } from './mediaController.js';

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
	const target = new EventTarget();
	const listen = vi.spyOn(target, 'addEventListener');
	const audio = Object.assign(target, { paused: true }) as unknown as HTMLAudioElement;
	const observations = {
		source: () => null, interrupt: vi.fn(), ended: vi.fn(async () => {}),
		seeking: vi.fn(), metadata: vi.fn(), position: vi.fn()
	};
	const attach = () => {
		listen.mockClear();
		const cleanup = media.attach(audio, observations);
		const signal = (listen.mock.calls[0]?.[2] as AddEventListenerOptions | undefined)?.signal;
		expect(signal?.aborted).toBe(false);
		for (const [, , options] of listen.mock.calls) expect(options).toMatchObject({ signal });
		return { cleanup, signal };
	};

	const first = attach();
	const replacement = attach();
	expect(first.signal?.aborted).toBe(true);
	first.cleanup();
	expect(replacement.signal?.aborted).toBe(false);
	replacement.cleanup();
	expect(replacement.signal?.aborted).toBe(true);

	const reset = attach();
	media.reset();
	expect(reset.signal?.aborted).toBe(true);
	const destroyed = attach();
	media.destroy();
	expect(destroyed.signal?.aborted).toBe(true);
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
