import { formatQueueM3U, formatQueueText, type QueueState } from '#lib/player/queue.js';
import { createM3UDownload, m3uDownloadFailure } from '#lib/player/m3uDownload.js';

export function createFavoritesExport(options: {
	isEmpty: () => boolean;
	snapshot: () => QueueState;
	currentKey: () => string;
	clearFeedback: () => void;
	setFeedback: (key: string, status: string, fallback?: string) => void;
}) {
	let generation = 0;
	let alive = true;
	const download = createM3UDownload();
	const unsafeId = 'Could not export favorites: a favorite has an unsafe track id.';

	function snapshotForExport(): QueueState {
		const snapshot = options.snapshot();
		if (snapshot.tracks.some((track) => typeof track.id !== 'string')) throw new Error(unsafeId);
		return snapshot;
	}

	function clear(): void {
		generation += 1;
		options.clearFeedback();
	}

	async function copy(): Promise<void> {
		clear();
		const capturedGeneration = generation;
		if (!alive || options.isEmpty()) return;
		const key = options.currentKey();
		let text: string;
		try {
			const snapshot = snapshotForExport();
			// The text formatter alone does not validate track IDs.
			formatQueueM3U(snapshot);
			text = formatQueueText(snapshot);
		} catch {
			options.setFeedback(key, unsafeId);
			return;
		}
		try {
			if (typeof navigator.clipboard?.writeText !== 'function') throw new Error('No clipboard');
			await navigator.clipboard.writeText(text);
		} catch {
			if (!alive || capturedGeneration !== generation || options.currentKey() !== key) return;
			options.setFeedback(key, 'Clipboard unavailable. Copy the favorites manually below.', text);
			return;
		}
		if (!alive || capturedGeneration !== generation || options.currentKey() !== key) return;
		options.setFeedback(key, 'Copied favorites as text.');
	}

	function downloadM3U(): void {
		clear();
		if (!alive || options.isEmpty()) return;
		const key = options.currentKey();
		let m3u: string;
		try {
			m3u = formatQueueM3U(snapshotForExport());
		} catch {
			options.setFeedback(key, unsafeId);
			return;
		}
		const link = document.createElement('a');
		try {
			const status = download.download(link, () => m3u, 'favorites.m3u', true);
			options.setFeedback(key, status);
		} catch {
			download.release();
			options.setFeedback(key, m3uDownloadFailure('favorites'));
		} finally {
			link.remove();
		}
	}

	function destroy(): void {
		alive = false;
		download.release();
	}

	return { clear, release: download.release, copy, downloadM3U, destroy };
}
