import { captureAuthFence } from '../../src/lib/auth-fence.js';
import { createBrowserPlaybackGrant, type BrowserPlaybackSource } from '../../src/lib/player/browserPlaybackGrant.js';
import { createListenEventSender } from '../../src/lib/player/listenEventSender.js';
import { createPlayerListenHistory } from '../../src/lib/player/playerListenHistory.js';

const trackId = 'synthetic-track';
const sessionBinding = 'A'.repeat(43);

declare global {
	interface Window {
		__nntuneListenEventFixture?: {
			open(playId: string): Promise<boolean>;
			playing(): void;
			ended(): void;
			close(): Promise<void>;
		};
	}
}

export async function mountListenEventFixture(signal: AbortSignal) {
	const owner = await captureAuthFence();
	if (!owner || signal.aborted) return;

	// Exercise the same receipt validation and publication as the browser
	// caller. The fixture supplies canonical synthetic IDs and private IDs.
	const grant = createBrowserPlaybackGrant(owner, sessionBinding, async (request) => ({
		...request, durationMs: 60_000
	}));
	let source: BrowserPlaybackSource | undefined;
	const sender = createListenEventSender({ owner, canRecord: () => true, grantForStart: () => source });
	const history = createPlayerListenHistory(sender);
	const audio = new Audio();
	history.attach(audio, trackId, () => source?.playId);
	const fixture: NonNullable<Window['__nntuneListenEventFixture']> = {
		open: (playId) => grant.open({ trackId, playId }, signal, () => true, (issued) => { source = issued; }),
		playing: () => { audio.dispatchEvent(new Event('playing')); },
		ended: () => { audio.dispatchEvent(new Event('ended')); },
		close() {
			history.dispose();
			return sender.close();
		}
	};
	signal.addEventListener('abort', () => {
		history.resetAtAuthBoundary();
		grant.dispose();
		if (window.__nntuneListenEventFixture === fixture) delete window.__nntuneListenEventFixture;
	}, { once: true });
	await sender.ready;
	if (!signal.aborted) window.__nntuneListenEventFixture = fixture;
}
