import { displayArtist, displayTitle } from '../library/order.js';
import { resolveStreamUrl } from '../library/stream-url.js';
import { clearMiniPlayerHint, setMiniPlayerHint } from '../shell/mini-player.js';
import { createMediaController, type MediaSelection } from './mediaController.js';
import { currentTrack, type QueueState } from './queue.js';
import { createQueueController, type LibrarySources, type QueueCommitKind } from './queueController.js';

type PlayerEffects = Pick<MediaSelection, 'flush'>;

type LibraryCommit = {
	sources: LibrarySources;
	isCurrent: () => boolean;
};

export function createPlayerController(
	initial: QueueState, effects: PlayerEffects, matchesNative?: (audio: HTMLAudioElement, source: string) => boolean
) {
	const queue = createQueueController(initial);
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
	const media = createMediaController(queue, { ...effects, browseCopy, commit: replaceQueue, announce: announceQueue }, matchesNative);

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
		// Retire queue and copy receipts before media destruction can call native pause.
		queue.destroy();
		browseCopy.invalidate();
		media.destroy();
	}

	function commitQueue(next: QueueState, kind: QueueCommitKind, library?: LibraryCommit): boolean {
		if ((library && !library.isCurrent()) || destroyed || queue.purging ||
			(!library && awaitingLibrary && next.tracks.length > 0)) return false;
		const receipt = queue.begin(next, kind, !!library);
		let version = media.attachmentVersion;
		// A callback can reset media without publishing a queue write (e.g. auth reset).
		const isCurrent = () => (!library || library.isCurrent()) &&
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
		if (trackChanged || resolveStreamUrl(before) !== resolveStreamUrl(after)) {
			// Sample the live element before retiring its queued volumechange events.
			media.observeSound();
			if (!isCurrent()) return false;
			version += 1;
			retireNativeHost();
			if (!isCurrent()) return false;
		}
		if (kind === 'replace') announceQueue('');
		if (!isCurrent()) return false;
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

	return {
		queue, media, browseCopy, get mediaRevision() { return mediaRevision; },
		get queueAnnouncement() { return queueAnnouncement; }, announceQueue,
		commitQueue: (next: QueueState, kind: QueueCommitKind) => commitQueue(next, kind),
		reconcileLibrary, replaceQueue, purgeAtAuthBoundary, destroy
	};
}
