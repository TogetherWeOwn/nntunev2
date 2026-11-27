import { expect, it, vi } from 'vitest';
import { notifyLocalAuthTransition } from '../auth-transition.js';
import { createPlayerListenHistory } from './playerListenHistory.js';

const playingAudio = () => Object.assign(new EventTarget(), {
	paused: false, seeking: false, ended: false, error: null
}) as unknown as HTMLAudioElement;

it('does not restart a consumed grant on the same or a replacement element', () => {
	const audio = playingAudio();
	const scope = { send: vi.fn(), close: vi.fn(), dispose: vi.fn() };
	const history = createPlayerListenHistory(scope, new EventTarget() as Window);
	const first = 'A'.repeat(22);
	let grant = first;
	const playAndFinish = (element: EventTarget) => {
		element.dispatchEvent(new Event('playing'));
		element.dispatchEvent(new Event('emptied'));
	};
	try {
		const detach = history.attach(audio, 'track', () => grant);
		for (grant of [first, `${first.slice(1)}Q`, first]) playAndFinish(audio);
		const beforeDetach = scope.send.mock.calls.map(([body]) => body.event);
		detach();
		const replacement = playingAudio();
		history.attach(replacement, 'track', () => grant);
		playAndFinish(replacement);
		const expected = ['start', 'skip', 'start', 'skip'];
		expect([beforeDetach, scope.send.mock.calls.map(([body]) => body.event)]).toEqual([expected, expected]);
	} finally {
		history.resetAtAuthBoundary();
	}
});

it('keeps auth cancellation armed throughout a terminal drain', async () => {
	const audio = playingAudio();
	const lifecycle = new EventTarget() as Window;
	const { promise, resolve } = Promise.withResolvers<void>();
	const scope = { send: vi.fn(), close: vi.fn(() => promise), dispose: vi.fn(resolve) };
	const history = createPlayerListenHistory(scope, lifecycle);

	try {
		history.attach(audio, 'track', () => 'A'.repeat(22));
		audio.dispatchEvent(new Event('playing'));
		history.dispose();
		expect(scope.send.mock.calls.map(([body]) => body.event)).toEqual(['start', 'skip']);
		expect(scope.close).toHaveBeenCalledOnce();
		notifyLocalAuthTransition();
		expect(scope.dispose).toHaveBeenCalledOnce();
		await promise;
		notifyLocalAuthTransition();
		expect(scope.dispose).toHaveBeenCalledOnce();
	} finally {
		history.resetAtAuthBoundary();
	}
});
