import { expect, it, vi } from 'vitest';
import { observeListenEvents, playbackIdFromGrant } from './observeListenEvents.js';

it('never credits a decoder failure as a completed listen', () => {
	const audio = Object.assign(new EventTarget(), { paused: false, seeking: false, ended: false, error: null as MediaError | null });
	const send = vi.fn();
	const observer = observeListenEvents(audio, { trackId: 'track', sender: { send },
		grantPlayId: () => 'A'.repeat(22), startedPlaybacks: new Set() });
	audio.dispatchEvent(new Event('playing'));
	audio.ended = true;
	audio.dispatchEvent(new Event('timeupdate'));
	audio.error = { code: 3 } as MediaError;
	audio.dispatchEvent(new Event('ended'));
	observer.dispose();
	expect(send.mock.calls.map(([event]) => event.event)).toEqual(['start', 'skip']);
	expect(playbackIdFromGrant(['A'.repeat(22)] as unknown as string)).toBeUndefined();
});
