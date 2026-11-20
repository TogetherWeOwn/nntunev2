import { expect, it, vi } from 'vitest';
import { observeListenEvents } from './observeListenEvents.js';

it('skips a started listen once on media error, but not a failed initial load', () => {
	const audio = new EventTarget();
	const send = vi.fn();
	let playback = 0;
	const observer = observeListenEvents(audio, {
		trackId: 'track',
		sender: { send },
		newId: () => String(++playback)
	});
	const fire = (name: string) => audio.dispatchEvent(new Event(name));
	fire('error');
	expect(send).not.toHaveBeenCalled();
	fire('playing');
	fire('error');
	fire('error');
	fire('emptied');
	fire('playing');
	observer.dispose();
	expect(send.mock.calls).toEqual([
		[{ playback_id: '1', track_id: 'track', event: 'start' }],
		[{ playback_id: '1', track_id: 'track', event: 'skip' }],
		[{ playback_id: '2', track_id: 'track', event: 'start' }],
		[{ playback_id: '2', track_id: 'track', event: 'skip' }]
	]);
});
