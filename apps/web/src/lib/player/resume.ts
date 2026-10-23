/**
 * Per-track resume positions. Serialized as a JSON object mapping track id to
 * seconds (`{"demo-track":42.5}`); all storage access is fail-soft.
 */

export const RESUME_STORAGE_KEY = 'nntune:resume:v1';

export interface ResumeStoreDeps {
	storage: Pick<Storage, 'getItem' | 'setItem' | 'removeItem'>;
	storageKey?: string;
}

export interface ResumeStore {
	/** Persist `seconds` for `trackId` (verbatim, no rounding). Ignores bad input. */
	save(trackId: string, seconds: number): void;
	/** Saved seconds, or null when absent, corrupt, or invalid. */
	positionFor(trackId: string): number | null;
	/** Drop the entry (no-op when absent). Called when a track ends. */
	clear(trackId: string): void;
}

function isValidPosition(value: unknown): value is number {
	return typeof value === 'number' && Number.isFinite(value) && value >= 0;
}

export function createResumeStore({
	storage,
	storageKey = RESUME_STORAGE_KEY
}: ResumeStoreDeps): ResumeStore {
	function readAll(): Record<string, number> {
		let raw: string | null;
		try {
			raw = storage.getItem(storageKey);
		} catch {
			return {};
		}
		if (raw === null) return {};
		try {
			const parsed: unknown = JSON.parse(raw);
			if (typeof parsed !== 'object' || parsed === null || Array.isArray(parsed)) return {};
			const positions: Record<string, number> = {};
			for (const [trackId, seconds] of Object.entries(parsed)) {
				if (isValidPosition(seconds)) positions[trackId] = seconds;
			}
			return positions;
		} catch {
			return {};
		}
	}

	function writeAll(positions: Record<string, number>): void {
		try {
			storage.setItem(storageKey, JSON.stringify(positions));
		} catch {
			// Fail-soft: playback continues from the start.
		}
	}

	return {
		save(trackId: string, seconds: number) {
			if (trackId === '' || !isValidPosition(seconds)) return;
			const positions = readAll();
			positions[trackId] = seconds;
			writeAll(positions);
		},
		positionFor(trackId: string) {
			return readAll()[trackId] ?? null;
		},
		clear(trackId: string) {
			const positions = readAll();
			if (!(trackId in positions)) return;
			delete positions[trackId];
			writeAll(positions);
		}
	};
}
