import { resolveStreamUrl } from '../library/stream-url.js';
import { createMediaController, type NaturalEndSelection } from './mediaController.js';
import { currentTrack, type QueueState } from './queue.js';
import { createQueueController, type LibrarySources, type QueueCommitKind } from './queueController.js';

type PlayerEffects = Omit<NaturalEndSelection, 'commit'> & {
	clearRestartFeedback: () => void;
	bumpMediaKey: () => void;
	clearQueueAnnouncement: () => void;
	syncMiniPlayerHint: (next: QueueState) => void;
};

type LibraryCommit = {
	sources: LibrarySources;
	isCurrent: () => boolean;
};

export function createPlayerController(
	initial: QueueState, effects: PlayerEffects, matchesNative?: (audio: HTMLAudioElement, source: string) => boolean
) {
	const queue = createQueueController(initial);
	const media = createMediaController(queue, { ...effects, commit: replaceQueue }, matchesNative);

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
			effects.clearRestartFeedback();
			if (!isCurrent()) return false;
		}
		if (trackChanged || resolveStreamUrl(before) !== resolveStreamUrl(after)) {
			effects.bumpMediaKey();
			if (!isCurrent()) return false;
			version += 1;
			media.reset();
			if (!isCurrent()) return false;
		}
		if (kind === 'replace') effects.clearQueueAnnouncement();
		if (!isCurrent()) return false;
		queue.publish(next, receipt, library?.sources);
		if (!isCurrent()) return false;
		effects.syncMiniPlayerHint(next);
		return isCurrent();
	}

	function replaceQueue(next: QueueState): boolean {
		return commitQueue(next, 'replace');
	}

	return { queue, media, commitQueue, replaceQueue };
}
