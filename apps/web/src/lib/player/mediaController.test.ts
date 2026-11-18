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
