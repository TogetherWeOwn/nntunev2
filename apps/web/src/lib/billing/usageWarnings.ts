/**
 * 80% / 95% usage warnings, once per threshold per month (spec §3.2).
 *
 * Shown-state lives client-side in `localStorage`, keyed
 * `period + threshold` — no server table, no email. A new period
 * re-arms every threshold naturally because the key changes. The keys
 * carry no account identity, so the auth boundary clears them
 * (`clearUsageWarnings`, called from `invalidateLibraryCache` on every
 * sign-in/sign-out): one account's shown-warning can never suppress
 * another account's warning on a shared browser. Storage is
 * best-effort: when it is unavailable (SSR, privacy mode, quota),
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

/** Enumerable storage for the auth-boundary clear (real `localStorage`). */
export interface ClearableWarningStorage extends WarningStorage {
	removeItem(key: string): void;
	key(index: number): string | null;
	readonly length: number;
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

/**
 * Drop every shown-warning flag, all periods and thresholds. Called at
 * the auth boundary (sign-in/sign-out via `invalidateLibraryCache`) so a
 * shared browser never carries one account's warning history into the
 * next account. Best-effort and never throws.
 */
export function clearUsageWarnings(
	storage: ClearableWarningStorage | null = defaultStorage() as ClearableWarningStorage | null
): void {
	if (!storage) return;
	try {
		const doomed: string[] = [];
		for (let i = 0; i < storage.length; i++) {
			const key = storage.key(i);
			if (key !== null && key.startsWith(`${KEY_PREFIX}:`)) doomed.push(key);
		}
		for (const key of doomed) storage.removeItem(key);
	} catch {
		/* best-effort; see module docs */
	}
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
