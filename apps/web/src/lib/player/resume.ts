/**
 * Per-track resume positions. Serialized as a JSON object mapping track id to
 * seconds (`{"demo-track":42.5}`); all storage access is fail-soft.
 *
 * Decoding is bounded (see `./localStateBounds.js`): raw payloads over
 * `MAX_SERIALIZED_CHARS` are refused without calling `JSON.parse`, parsed
 * objects over `MAX_RESUME_ENTRIES` own keys are refused wholesale, and ids
 * over `MAX_OPAQUE_ID_CHARS` code units are treated as absent. Ids are opaque
 * own keys stored in a null-prototype map, so `__proto__`, `constructor` and
 * friends round-trip as data without reading inherited members or mutating
 * prototypes.
 */

import {
	MAX_OPAQUE_ID_CHARS,
	MAX_RESUME_ENTRIES,
	MAX_SERIALIZED_CHARS
} from './localStateBounds.js';

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
	function emptyPositions(): Record<string, number> {
		return Object.create(null);
	}

	function readAll(): Record<string, number> {
		let raw: string | null;
		try {
			raw = storage.getItem(storageKey);
		} catch {
			return emptyPositions();
		}
		if (raw === null) return emptyPositions();
		if (raw.length > MAX_SERIALIZED_CHARS) return emptyPositions();
		try {
			const parsed: unknown = JSON.parse(raw);
			if (typeof parsed !== 'object' || parsed === null || Array.isArray(parsed)) {
				return emptyPositions();
			}
			const entries = Object.entries(parsed);
			if (entries.length > MAX_RESUME_ENTRIES) return emptyPositions();
			const positions: Record<string, number> = emptyPositions();
			for (const [trackId, seconds] of entries) {
				if (trackId.length > MAX_OPAQUE_ID_CHARS) continue;
				if (isValidPosition(seconds)) positions[trackId] = seconds;
			}
			return positions;
		} catch {
			return emptyPositions();
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
			if (trackId === '' || trackId.length > MAX_OPAQUE_ID_CHARS) return;
			if (!isValidPosition(seconds)) return;
			const positions = readAll();
			const isOwn = Object.prototype.hasOwnProperty.call(positions, trackId);
			if (!isOwn && Object.keys(positions).length >= MAX_RESUME_ENTRIES) return;
			positions[trackId] = seconds;
			let serialized: string;
			try {
				serialized = JSON.stringify(positions);
			} catch {
				return;
			}
			if (serialized.length > MAX_SERIALIZED_CHARS) return;
			try {
				storage.setItem(storageKey, serialized);
			} catch {
				// Fail-soft: playback continues from the start.
			}
		},
		positionFor(trackId: string) {
			const positions = readAll();
			if (!Object.prototype.hasOwnProperty.call(positions, trackId)) return null;
			return positions[trackId] ?? null;
		},
		clear(trackId: string) {
			const positions = readAll();
			if (!Object.prototype.hasOwnProperty.call(positions, trackId)) return;
			delete positions[trackId];
			writeAll(positions);
		}
	};
}
