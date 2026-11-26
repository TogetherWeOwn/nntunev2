import { AUTH_LOCK, captureAuthFence } from '../../src/lib/auth-fence.js';
import { createBrowserPlaybackGrant, type BrowserPlaybackSource } from '../../src/lib/player/browserPlaybackGrant.js';
import { createListenEventSender } from '../../src/lib/player/listenEventSender.js';
import { createPlayerListenHistory } from '../../src/lib/player/playerListenHistory.js';

const trackId = 'synthetic-track';
const sessionBinding = 'A'.repeat(43);

interface ListenEventFixture {
	open(playId: string): Promise<boolean>;
	playing(): void;
	ended(): void;
	writesAfterPendingDispatch(): Promise<number>;
	close(): Promise<void>;
}

declare global {
	interface Window {
		__nntuneListenEventFixture?: Promise<ListenEventFixture | undefined>;
	}
}

export async function mountListenEventFixture(signal: AbortSignal): Promise<ListenEventFixture | undefined> {
	const owner = await captureAuthFence();
	if (!owner || signal.aborted) return;

	// Exercise the same receipt validation and publication as the browser
	// caller. The fixture supplies canonical synthetic IDs and private IDs.
	const grant = createBrowserPlaybackGrant(owner, sessionBinding, async (request) => ({
		...request, durationMs: 60_000
	}));
	let source: BrowserPlaybackSource | undefined;
	let writes = 0;
	const sender = createListenEventSender({
		owner, canRecord: () => true, grantForStart: () => source,
		fetchFn(input, init) {
			if (input === '/v1/plays/events') writes++;
			return fetch(input, init);
		}
	});
	const history = createPlayerListenHistory(sender);
	const audio = new Audio();
	history.attach(audio, trackId, () => source?.playId);
	signal.addEventListener('abort', () => {
		history.resetAtAuthBoundary();
		grant.dispose();
	}, { once: true });
	await sender.ready;
	if (signal.aborted) return;
	return {
		open: (playId) => grant.open({ trackId, playId }, signal, () => true, (issued) => { source = issued; }),
		playing: () => { audio.dispatchEvent(new Event('playing')); },
		ended: () => { audio.dispatchEvent(new Event('ended')); },
		// Wait behind any sender dispatch already queued on the account fence.
		writesAfterPendingDispatch: () => navigator.locks.request(AUTH_LOCK, () => writes),
		close() {
			history.dispose();
			return sender.close();
		}
	};
}
