import { expect, it, vi } from 'vitest';
import { notifyLocalAuthTransition } from '../auth-transition.js';
import { createPlayerListenHistory } from './playerListenHistory.js';

it('keeps auth cancellation armed throughout a terminal drain', async () => {
	const audio = new EventTarget() as HTMLAudioElement;
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
