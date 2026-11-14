import { currentTrack, type QueueState } from './queue.js';
import { buildShareQuery } from './shareTimestamp.js';
import { resolveStreamUrl } from '../library/stream-url.js';

interface QueueShareDeps {
	getQueue: () => QueueState;
	getAudio: () => HTMLAudioElement | null;
	getOrigin: () => string;
	getClipboard: () => { writeText: (text: string) => Promise<void> } | undefined;
	onOutcome: (status: string, fallbackUrl: string | null) => void;
}

/** One session's row-copy intent, independent of the route that activated it. */
export function createQueueShare(deps: QueueShareDeps) {
	let generation = 0;

	async function copy(id: string): Promise<void> {
		const action = ++generation;
		const active = () => action === generation;
		deps.onOutcome('', null);
		if (!active()) return;
		const queue = deps.getQueue();
		if (!active()) return;
		const queued = queue.tracks.find((track) => track.id === id);
		if (!queued) {
			deps.onOutcome('Link unavailable — that track is no longer in the queue.', null);
			return;
		}
		const points = Array.from(queued.title?.trim() ? queued.title : queued.id);
		const title = points.length > 70 ? `${points.slice(0, 69).join('')}…` : points.join('');
		const unavailable = `Link unavailable — ${title} is no longer in the queue.`;
		const outcome = (status: string, fallbackUrl: string | null) => {
			if (!active()) return;
			const remains = deps.getQueue().tracks.some((track) => track.id === id);
			if (!active()) return;
			if (!remains) deps.onOutcome(unavailable, null);
			else deps.onOutcome(status, fallbackUrl);
		};
		let url: string;
		try {
			const current = currentTrack(queue);
			const position = current?.id === id && resolveStreamUrl(current) ? deps.getAudio()?.currentTime : 0;
			const seconds = typeof position === 'number' && Number.isFinite(position) && position >= 0 ? position : 0;
			url = `${deps.getOrigin()}/library?${buildShareQuery(id, seconds)}`;
		} catch {
			outcome(unavailable, null);
			return;
		}
		if (!active()) return;
		let write: ((text: string) => Promise<void>) | undefined;
		try {
			const clipboard = deps.getClipboard();
			const method = clipboard?.writeText;
			if (typeof method === 'function') write = (text) => method.call(clipboard, text);
		} catch { /* Manual copy remains usable. */ }
		if (!active()) return;
		if (!write) {
			outcome('', url);
			return;
		}
		let written = true;
		try { await write(url); } catch { written = false; }
		outcome(written ? `Copied link for ${title}.` : '', written ? null : url);
	}

	return { copy, reset: () => { ++generation; } };
}
