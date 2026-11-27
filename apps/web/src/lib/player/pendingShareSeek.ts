import { isShareTimestamp } from './shareTimestamp.js';

export type ShareSeekTarget = { trackId: string; streamUrl: string; seconds: number };
type Pending = ShareSeekTarget & {
	appliedSeconds: number | null;
	capture: (element: HTMLAudioElement) => Receipt;
};
type Receipt = { isCurrent: () => boolean; cancelResume?: () => void };

export function createPendingShareSeek() {
	let pending: Pending | null = null;
	let revision = 0;
	let writing = false;
	function clear() { pending = null; revision += 1; }
	return {
		get current(): Readonly<Omit<Pending, 'capture'>> | null { return pending; },
		get revision() { return revision; },
		set(target: ShareSeekTarget, capture: Pending['capture']) {
			pending = isShareTimestamp(target.seconds) ? { ...target, capture, appliedSeconds: null } : null;
			return ++revision;
		},
		clear,
		supersede(trackId: string, source: string) {
			if (pending?.trackId === trackId && pending.streamUrl === source) clear();
		},
		apply(element: HTMLAudioElement | undefined | null, expectedRevision = revision, hasRetry?: () => boolean) {
			const target = pending;
			if (!target || !element || expectedRevision !== revision) return;
			try {
				const receipt = target.capture(element);
				const current = () => pending === target && revision === expectedRevision &&
					receipt.isCurrent() && pending === target && revision === expectedRevision;
				if (!current()) return;
				const duration = element.duration;
				if (!current()) return;
				const known = Number.isFinite(duration) && duration >= 0;
				const seconds = known ? Math.min(target.seconds, duration) : target.seconds;
				receipt.cancelResume?.();
				if (!current()) return;
				target.appliedSeconds = seconds;
				writing = true;
				try { element.currentTime = seconds; } finally { writing = false; }
				if (known && current() && !hasRetry?.()) clear();
			} catch {
				// Native getters and setters may refuse until metadata is available.
			}
		},
		observeSeeking(element: HTMLAudioElement) {
			const target = pending, expectedRevision = revision;
			if (writing || !target || target.appliedSeconds == null) return;
			try {
				const receipt = target.capture(element);
				const current = () => pending === target && revision === expectedRevision &&
					receipt.isCurrent() && pending === target && revision === expectedRevision;
				if (!current()) return;
				const position = element.currentTime;
				if (current() && position !== target.appliedSeconds) clear();
			} catch { /* retry later */ }
		}
	};
}
