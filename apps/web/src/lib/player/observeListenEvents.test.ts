import { expect, it, vi } from 'vitest';
import { observeListenEvents } from './observeListenEvents.js';

// Canonical unpadded base64url encodings of the same 16 bytes as each UUID.
const grant = 'QMMlwCzuSJ2otvvHUxVWAA';
const nextGrant = 'QMMlwCzuSJ2otvvHUxVWAQ';
const playback = '40c325c0-2cee-489d-a8b6-fbc753155600';
const nextPlayback = '40c325c0-2cee-489d-a8b6-fbc753155601';

function fixture() {
	const audio = new EventTarget();
	const send = vi.fn();
	let id: string | undefined;
	const observer = observeListenEvents(audio, {
		trackId: 'track',
		sender: { send },
		grantPlayId: () => id
	});
	return {
		observer,
		useGrant: (playId: string) => { id = playId; },
		fire: (name: string) => audio.dispatchEvent(new Event(name)),
		bodies: () => send.mock.calls.map(([body]) => body)
	};
}

it('skips a started listen once on media error, but not a failed initial load', () => {
	const { observer, useGrant, fire, bodies } = fixture();
	useGrant(grant);
	fire('error');
	expect(bodies()).toEqual([]);
	fire('playing');
	fire('error');
	fire('error');
	fire('emptied');
	fire('playing'); // The ended grant cannot start another listen.
	useGrant(nextGrant);
	fire('playing');
	observer.dispose();
	expect(bodies()).toEqual([
		{ playback_id: playback, track_id: 'track', event: 'start' },
		{ playback_id: playback, track_id: 'track', event: 'skip' },
		{ playback_id: nextPlayback, track_id: 'track', event: 'start' },
		{ playback_id: nextPlayback, track_id: 'track', event: 'skip' }
	]);
});

it('ignores missing or invalid grants and ends against the grant captured at start', () => {
	const { useGrant, fire, bodies } = fixture();
	fire('playing');
	useGrant(`${grant.slice(0, -1)}B`); // Noncanonical padding bits.
	fire('playing');
	expect(bodies()).toEqual([]);
	useGrant(grant);
	fire('playing');
	useGrant(nextGrant);
	fire('ended');
	expect(bodies()).toEqual([
		{ playback_id: playback, track_id: 'track', event: 'start' },
		{ playback_id: playback, track_id: 'track', event: 'complete' }
	]);
});
