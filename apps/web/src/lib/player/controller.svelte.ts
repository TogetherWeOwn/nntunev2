import { displayArtist, displayTitle } from '../library/order.js';
import { resolveStreamUrl } from '../library/stream-url.js';
import { clearMiniPlayerHint, setMiniPlayerHint } from '../shell/mini-player.js';
import { createMediaController, type MediaSelection } from './mediaController.js';
import { currentTrack, type QueueState } from './queue.js';
import { createQueueController, type LibrarySources, type QueueCommitKind } from './queueController.js';

type PlayerEffects = Pick<MediaSelection, 'flush' | 'clearShareFeedback'>;

type LibraryCommit = {
	sources: LibrarySources;
	isCurrent: () => boolean;
};

export function createPlayerController(
	initial: QueueState, effects: PlayerEffects, matchesNative?: (audio: HTMLAudioElement, source: string) => boolean
) {
	const queue = createQueueController(initial);
	let queueAnnouncement = $state.raw<Readonly<{ text: string; version: number }>>({ text: '', version: 0 });
	const media = createMediaController(queue, { ...effects, commit: replaceQueue, announce: announceQueue }, matchesNative);

	// Keyed messages can announce identical text again without replacing the live region.
	function announceQueue(text: string, replay = false): void {
		if (text === queueAnnouncement.text && !replay) return;
		queueAnnouncement = { text, version: queueAnnouncement.version + Number(replay) };
	}

	// A source round trip before Svelte flushes must not revive a retired node.
	// This keys the render, independently of attachment and detach receipts.
	let mediaRevision = $state(0);

	function retireNativeHost(): void {
		mediaRevision += 1;
		media.reset();
	}

	function resetAtAuthBoundary(): void {
		media.clearShareSeek();
		retireNativeHost();
		media.clearSeekFeedback();
	}

	function commitQueue(next: QueueState, kind: QueueCommitKind, library?: LibraryCommit): boolean {
		if (library && !library.isCurrent()) return false;
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
			if (kind === 'replace') effects.clearShareFeedback();
			if (!isCurrent()) return false;
			media.clearShareSeek();
			if (!isCurrent()) return false;
		}
		if (rawSourceChanged && (kind === 'replace' || kind === 'remove' || kind === 'select')) {
			media.clearSeekFeedback();
			if (!isCurrent()) return false;
		}
		if (trackChanged || resolveStreamUrl(before) !== resolveStreamUrl(after)) {
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

	function replaceQueue(next: QueueState): boolean {
		return commitQueue(next, 'replace');
	}

	return {
		queue, media, get mediaRevision() { return mediaRevision; },
		get queueAnnouncement() { return queueAnnouncement; }, announceQueue,
		commitQueue, replaceQueue, resetAtAuthBoundary
	};
}
