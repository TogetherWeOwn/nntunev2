import { displayArtist, displayTitle } from '../library/order.js';
import { resolveStreamUrl } from '../library/stream-url.js';
import { clearMiniPlayerHint, setMiniPlayerHint } from '../shell/mini-player.js';
import { validSessionBinding, type AccountSession } from '../auth-fence.js';
import type { BrowserPlaybackSource } from './browserPlaybackGrant.js';
import { createBrowserPlaybackSession, type PrivateListeningState } from './browserPlaybackSession.js';
import { createMediaController, type MediaSelection } from './mediaController.js';
import { clearQueue, currentTrack, moveTrack, removeTrack, type QueueState, type Track } from './queue.js';
import { createQueueController, type LibrarySources, type QueueCommitKind } from './queueController.js';

type PlayerEffects = Pick<MediaSelection, 'flush'>;

type LibraryCommit = {
	sources: LibrarySources;
	isCurrent: () => boolean;
};

export function createPlayerController(
	initial: QueueState, effects: PlayerEffects, matchesNative?: (audio: HTMLAudioElement, source: string) => boolean,
	browserPlayback?: { fetchFn?: typeof fetch }
) {
	const queue = createQueueController(initial);
	let browserSession: ReturnType<typeof createBrowserPlaybackSession> | undefined;
	let privateListening = $state.raw<PrivateListeningState>();
	let activeGrant = $state.raw<BrowserPlaybackSource>();
	function streamFor(track: Track | null): string {
		if (!browserPlayback) return resolveStreamUrl(track);
		return activeGrant && activeGrant.trackId === track?.id && activeGrant.isCurrent() ? activeGrant.streamUrl : '';
	}
	function retireBrowserPlayback() {
		const previous = browserSession;
		browserSession = undefined;
		previous?.owner.signal.removeEventListener('abort', purgeAtAuthBoundary);
		previous?.dispose();
		activeGrant = privateListening = undefined;
	}
	function bindBrowserPlayback(owner: AccountSession, binding: string) {
		if (!browserPlayback || destroyed || queue.purging || !owner.isCurrent() || !validSessionBinding(binding) ||
			(browserSession?.owner === owner && browserSession.binding === binding)) return;
		if (browserSession) purgeAtAuthBoundary();
		if (destroyed || !owner.isCurrent()) return;
		browserSession = createBrowserPlaybackSession(owner, binding, (state) => { privateListening = state; }, browserPlayback.fetchFn);
		privateListening = { enabled: false, pending: false, feedback: '' };
		owner.signal.addEventListener('abort', purgeAtAuthBoundary, { once: true });
	}
	let queueAnnouncement = $state.raw<Readonly<{ text: string; version: number }>>({ text: '', version: 0 });
	let destroyed = false;
	let accountGeneration = 0;
	let awaitingLibrary = false;
	let browseCopyGeneration = 0;
	let browseCopyFeedback = $state.raw<Readonly<{ status: string; fallbackUrl: string | null }>>({ status: '', fallbackUrl: null });
	const browseCopy = {
		get feedback() { return browseCopyFeedback; },
		get generation() { return browseCopyGeneration; },
		invalidate(): number {
			const generation = ++browseCopyGeneration;
			browseCopyFeedback = { status: '', fallbackUrl: null };
			return generation;
		},
		settle(generation: number, status: string, fallbackUrl: string | null): void {
			if (!destroyed && !queue.purging && generation === browseCopyGeneration) browseCopyFeedback = { status, fallbackUrl };
		}
	};
	const media = createMediaController(queue, {
		...effects, browseCopy, streamFor, announce: announceQueue,
		commit: (next, source) => commitQueue(next, 'replace', undefined, source),
		prepare: browserPlayback ? (id, current, publish, retry) => {
			const playId = retry && activeGrant?.trackId === id ? activeGrant.playId : undefined;
			if (retry && !playId) return Promise.resolve(false);
			return browserSession?.open(id, current, publish, playId) ?? Promise.resolve(false);
		} : undefined
	}, matchesNative);

	// Keyed messages can announce identical text again without replacing the live region.
	function announceQueue(text: string, replay = false): void {
		if (destroyed || queue.purging || (text === queueAnnouncement.text && !replay)) return;
		queueAnnouncement = { text, version: queueAnnouncement.version + Number(replay) };
	}

	// A source round trip before Svelte flushes must not revive a retired node.
	// This keys the render, independently of attachment and detach receipts.
	let mediaRevision = $state(0);

	function retireNativeHost(): void {
		mediaRevision += 1;
		media.reset();
	}

	function purgeAtAuthBoundary(): void {
		if (destroyed || queue.purging) return;
		accountGeneration += 1;
		retireBrowserPlayback();
		// Svelte can deliver nested queue notifications after the purge returns.
		// Don't accept new queue entries until fresh Library evidence arrives.
		awaitingLibrary = true;
		queue.purgeAtAuthBoundary(() => {
			browseCopy.invalidate();
			if (queueAnnouncement.text) queueAnnouncement = { ...queueAnnouncement, text: '' };
			mediaRevision += 1;
			media.purgeAtAuthBoundary(clearMiniPlayerHint);
		});
	}

	function destroy(): void {
		if (destroyed) return;
		destroyed = true;
		retireBrowserPlayback();
		// Retire queue and copy receipts before media destruction can call native pause.
		queue.destroy();
		browseCopy.invalidate();
		media.destroy();
	}

	function commitQueue(next: QueueState, kind: QueueCommitKind, library?: LibraryCommit, grant?: BrowserPlaybackSource): boolean {
		if ((library && !library.isCurrent()) || grant?.isCurrent() === false || destroyed || queue.purging ||
			(!library && awaitingLibrary && next.tracks.length > 0)) return false;
		const receipt = queue.begin(next, kind, !!library);
		let version = media.attachmentVersion;
		// A callback can reset media without publishing a queue write (e.g. auth reset).
		const isCurrent = () => (!library || library.isCurrent()) && grant?.isCurrent() !== false &&
			queue.isCurrentOperation(receipt) && media.attachmentVersion === version;
		if (library && next === queue.snapshot) {
			queue.publish(next, receipt, library.sources);
			return isCurrent();
		}
		const before = currentTrack(queue.snapshot);
		const after = currentTrack(next);
		const trackChanged = before?.id !== after?.id;
		const rawSourceChanged = trackChanged || before?.streamUrl !== after?.streamUrl;
		if (rawSourceChanged && (kind === 'replace' || kind === 'select')) {
			if (kind === 'replace') browseCopy.invalidate();
			if (!isCurrent()) return false;
			media.clearShareSeek();
			if (!isCurrent()) return false;
		}
		if (rawSourceChanged && (kind === 'replace' || kind === 'remove' || kind === 'select')) {
			media.clearSeekFeedback();
			if (!isCurrent()) return false;
		}
		const nextSource = grant?.streamUrl ?? streamFor(after);
		if (trackChanged || streamFor(before) !== nextSource) {
			// Sample the live element before retiring its queued volumechange events.
			media.observeSound();
			if (!isCurrent()) return false;
			const rate = media.speed.observe();
			if (!isCurrent()) return false;
			version += 1;
			retireNativeHost();
			if (!isCurrent()) return false;
			if (!nextSource) media.sleepTimer.cancel();
			else media.speed.handoff(() => queue.isCurrentOperation(receipt) && queue.snapshot === next &&
				media.attachmentVersion === version + 1 ? rate : null);
		}
		if (kind === 'replace') announceQueue('');
		if (!isCurrent()) return false;
		if (grant || trackChanged) activeGrant = grant;
		queue.publish(next, receipt, library?.sources);
		if (!isCurrent()) return false;
		const selected = currentTrack(queue.snapshot);
		if (selected) {
			setMiniPlayerHint({ trackId: selected.id, title: displayTitle(selected), artist: displayArtist(selected) });
		} else {
			clearMiniPlayerHint();
		}
		// Hint listeners run synchronously and can retire this operation too.
		return isCurrent();
	}

	function reconcileLibrary(
		items: Parameters<typeof queue.prepareLibrary>[0], isCurrent: () => boolean,
		beforeCommit?: (queueChanged: boolean) => void
	): boolean {
		const generation = accountGeneration;
		const ownsSnapshot = () => !queue.purging && isCurrent() && !destroyed && generation === accountGeneration;
		if (!ownsSnapshot()) return false;
		const { next, sources } = queue.prepareLibrary(items);
		// The caller's UI work runs inline; the guarded commit rechecks both receipts afterward.
		beforeCommit?.(next !== queue.snapshot);
		if (!commitQueue(next, 'replace', { sources, isCurrent: ownsSnapshot })) return false;
		if (sources.size) awaitingLibrary = false;
		return true;
	}

	function replaceQueue(next: QueueState): boolean {
		return commitQueue(next, 'replace');
	}

	// These callbacks edit the live snapshot synchronously. The renderer owns
	// gesture validation, confirmation and focus recovery.
	function moveQueuedTrack(from: number, to: number): boolean {
		const state = queue.snapshot;
		const moved = state.tracks[from];
		if (moved === undefined) return false;
		const next = moveTrack(state, from, to);
		if (next === state || !commitQueue(next, 'move')) return false;
		announceQueue(`${moved.title} moved to ${to + 1} of ${next.tracks.length}`);
		return true;
	}

	function removeQueuedTrack(index: number): boolean {
		const state = queue.snapshot;
		const removed = state.tracks[index];
		if (removed === undefined) return false;
		const next = removeTrack(state, index);
		if (next === state) return false;
		const title = removed.title?.trim() ? removed.title : removed.id;
		if (!commitQueue(next, 'remove')) return false;
		// Distinct rows with equal titles still replay in the stable live region.
		announceQueue(`${title} removed from the queue.`, true);
		return true;
	}

	function clearQueuedTracks(): boolean {
		const state = queue.snapshot;
		const next = clearQueue(state);
		if (next === state || !commitQueue(next, 'clear')) return false;
		announceQueue('Queue cleared.', true);
		return true;
	}

	return {
		moveQueuedTrack, removeQueuedTrack, clearQueuedTracks,
		streamFor, bindBrowserPlayback, get privateListening() { return privateListening; },
		setPrivateListening: (enabled: boolean) => browserSession?.setPrivate(enabled),
		queue, media, browseCopy, get mediaRevision() { return mediaRevision; },
		get queueAnnouncement() { return queueAnnouncement; }, announceQueue,
		commitQueue: (next: QueueState, kind: QueueCommitKind) => commitQueue(next, kind),
		reconcileLibrary, replaceQueue, purgeAtAuthBoundary, destroy
	};
}
