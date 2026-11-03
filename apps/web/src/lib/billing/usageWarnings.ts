/**
 * 80% / 95% usage warnings, once per threshold per month (spec §3.2).
 *
 * Shown-state lives client-side in `localStorage`, keyed
 * `period + threshold` — no server table, no email. A new period
 * re-arms every threshold naturally because the key changes. Storage
 * is best-effort: when it is unavailable (SSR, privacy mode, quota),
 * warnings simply do not fire — meter display never depends on this
 * module, and a missing warning is a copy nicety, not an enforcement
 * gap (the server refuses at the cap regardless).
 */

export const WARNING_THRESHOLDS = [80, 95] as const;

export type WarningThreshold = (typeof WARNING_THRESHOLDS)[number];

const KEY_PREFIX = 'nntune:usage-warning';

function keyFor(period: string, threshold: WarningThreshold): string {
	return `${KEY_PREFIX}:${period}:${threshold}`;
}

export interface WarningStorage {
	getItem(key: string): string | null;
	setItem(key: string, value: string): void;
}

function defaultStorage(): WarningStorage | null {
	try {
		if (typeof localStorage === 'undefined') return null;
		return localStorage;
	} catch {
		return null;
	}
}

/**
 * Thresholds crossed at `streamFraction` that have not yet been shown
 * this period, ascending. Crossing both in one render (e.g. first visit
 * already at 97%) returns both — callers show the highest and mark all
 * returned thresholds shown, so the lower one never fires late.
 */
export function pendingUsageWarnings(
	period: string,
	streamFraction: number,
	storage: WarningStorage | null = defaultStorage()
): WarningThreshold[] {
	if (!storage || !period) return [];
	const pct = streamFraction * 100;
	const pending: WarningThreshold[] = [];
	for (const threshold of WARNING_THRESHOLDS) {
		if (pct < threshold) continue;
		try {
			if (storage.getItem(keyFor(period, threshold)) === null) pending.push(threshold);
		} catch {
			/* unreadable storage: treat as already shown, never spam */
		}
	}
	return pending;
}

/** Record thresholds as shown for this period (call after displaying). */
export function markUsageWarningsShown(
	period: string,
	thresholds: readonly WarningThreshold[],
	storage: WarningStorage | null = defaultStorage()
): void {
	if (!storage || !period) return;
	for (const threshold of thresholds) {
		try {
			storage.setItem(keyFor(period, threshold), '1');
		} catch {
			/* best-effort; see module docs */
		}
	}
}
