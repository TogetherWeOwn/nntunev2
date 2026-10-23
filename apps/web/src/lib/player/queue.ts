/**
 * Playback queue: pure, immutable state transitions so the panel, the
 * transport buttons and the MediaSession handlers all share one model.
 * Navigation clamps at both ends (no wrap): `next` on the last track is
 * where playback ends, which is what the `ended` event relies on.
 */

export interface Track {
	id: string;
	title: string;
	artist: string;
	album?: string;
	streamUrl: string;
	/** Null/missing artwork goes through the shared artwork fallback. */
	artworkUrl?: string | null;
}

export interface QueueState {
	tracks: Track[];
	/** Index into `tracks`, or -1 when the queue is empty. */
	currentIndex: number;
}

/**
 * Three-track fixture for the demo player and the queue smoke tests.
 * Track 1 keeps the plain `/api/demo/audio` URL (player-seek.spec.ts
 * asserts it verbatim) and deliberately has no artwork, so the shared
 * placeholder fallback is what renders and reaches MediaSession.
 */
export const DEMO_TRACKS: Track[] = [
	{
		id: 'demo-track',
		title: 'Demo track',
		artist: 'Fixture Band',
		streamUrl: '/api/demo/audio',
		artworkUrl: null
	},
	{
		id: 'second-signal',
		title: 'Second Signal',
		artist: 'Fixture Band',
		streamUrl: '/api/demo/audio?track=second-signal',
		artworkUrl: '/api/demo/artwork'
	},
	{
		id: 'third-light',
		title: 'Third Light',
		artist: 'Session Players',
		streamUrl: '/api/demo/audio?track=third-light',
		artworkUrl: '/api/demo/artwork'
	}
];

export function createQueue(tracks: readonly Track[]): QueueState {
	return { tracks: [...tracks], currentIndex: tracks.length > 0 ? 0 : -1 };
}

export function currentTrack(state: QueueState): Track | null {
	return state.tracks[state.currentIndex] ?? null;
}

export function nextTrack(state: QueueState): QueueState {
	if (state.currentIndex < 0 || state.currentIndex >= state.tracks.length - 1) return state;
	return { ...state, currentIndex: state.currentIndex + 1 };
}

export function previousTrack(state: QueueState): QueueState {
	if (state.currentIndex <= 0) return state;
	return { ...state, currentIndex: state.currentIndex - 1 };
}

export function selectTrack(state: QueueState, index: number): QueueState {
	if (!Number.isInteger(index) || index < 0 || index >= state.tracks.length) return state;
	return { ...state, currentIndex: index };
}

/** Append a track (by id, duplicates ignored). Never steals the current track. */
export function addTrack(state: QueueState, track: Track): QueueState {
	if (state.tracks.some((t) => t.id === track.id)) return state;
	return {
		tracks: [...state.tracks, track],
		currentIndex: state.currentIndex < 0 ? 0 : state.currentIndex
	};
}

export function clearQueue(state: QueueState): QueueState {
	if (state.tracks.length === 0) return state;
	return { tracks: [], currentIndex: -1 };
}

/**
 * Move the track at `from` to `to`. The current *track* (not the slot)
 * stays current, so reordering never restarts or skips playback.
 */
export function moveTrack(state: QueueState, from: number, to: number): QueueState {
	const len = state.tracks.length;
	if (
		!Number.isInteger(from) ||
		!Number.isInteger(to) ||
		from < 0 ||
		to < 0 ||
		from >= len ||
		to >= len ||
		from === to
	) {
		return state;
	}
	const currentId = currentTrack(state)?.id ?? null;
	const tracks = [...state.tracks];
	const [moved] = tracks.splice(from, 1);
	tracks.splice(to, 0, moved!);
	return {
		tracks,
		currentIndex: currentId === null ? state.currentIndex : tracks.findIndex((t) => t.id === currentId)
	};
}

/** One-shot Fisher–Yates over the upcoming suffix; prefix/current stay fixed. */
export function shuffleUpcoming(state: QueueState, random: () => number = Math.random): QueueState {
	const start = state.currentIndex + 1;
	if (state.currentIndex < 0 || state.tracks.length - start < 2) return state;
	const tracks = [...state.tracks];
	for (let i = tracks.length - 1; i > start; i -= 1) {
		const j = start + Math.floor(random() * (i - start + 1));
		[tracks[i], tracks[j]] = [tracks[j]!, tracks[i]!];
	}
	return { ...state, tracks };
}

/**
 * Remove the track at `index`. The current *track* (not the slot) stays
 * current: removing ahead of it shifts the index down, removing it advances
 * playback to the following track (or the new last track when the tail
 * played), and removing the only track empties the queue.
 */
export function removeTrack(state: QueueState, index: number): QueueState {
	const len = state.tracks.length;
	if (!Number.isInteger(index) || index < 0 || index >= len) return state;
	const tracks = [...state.tracks];
	tracks.splice(index, 1);
	if (tracks.length === 0) return { tracks, currentIndex: -1 };
	let currentIndex = state.currentIndex;
	if (index < currentIndex) currentIndex -= 1;
	else if (index === currentIndex) currentIndex = Math.min(currentIndex, tracks.length - 1);
	return { tracks, currentIndex };
}

/**
 * Move the track at `index` to play immediately after the current track,
 * reusing `moveTrack` so the current track identity is preserved and the
 * track is never duplicated. A no-op on the current track itself.
 */
export function playNext(state: QueueState, index: number): QueueState {
	const len = state.tracks.length;
	if (!Number.isInteger(index) || index < 0 || index >= len) return state;
	if (state.currentIndex < 0 || index === state.currentIndex) return state;
	const to = index < state.currentIndex ? state.currentIndex : state.currentIndex + 1;
	return moveTrack(state, index, to);
}

/** In-session persistence (sessionStorage): order + current track id only. */
export function serializeQueue(state: QueueState): string {
	return JSON.stringify({
		orderIds: state.tracks.map((t) => t.id),
		currentId: currentTrack(state)?.id ?? null
	});
}

/**
 * Rebuild a queue from `serializeQueue` output against the known track
 * catalogue. Anything unrecognizable falls back to the full catalogue
 * queue, except an explicitly cleared queue (empty `orderIds`), which
 * stays cleared. Repeated ids are unrecognizable too: the panel keys on
 * `track.id` and `moveTrack` locates the current track by id, so a
 * duplicate would break both.
 */
export function restoreQueue(raw: string | null, catalogue: readonly Track[]): QueueState {
	const fallback = createQueue(catalogue);
	if (raw === null) return fallback;
	try {
		const parsed = JSON.parse(raw) as { orderIds?: unknown; currentId?: unknown };
		if (!Array.isArray(parsed.orderIds) || !parsed.orderIds.every((id) => typeof id === 'string')) {
			return fallback;
		}
		if (parsed.orderIds.length === 0) return { tracks: [], currentIndex: -1 };
		if (new Set(parsed.orderIds).size !== parsed.orderIds.length) return fallback;
		const byId = new Map(catalogue.map((t) => [t.id, t]));
		const tracks = parsed.orderIds.map((id) => byId.get(id as string));
		if (tracks.some((t) => t === undefined)) return fallback;
		const list = tracks as Track[];
		const currentIndex =
			typeof parsed.currentId === 'string' ? list.findIndex((t) => t.id === parsed.currentId) : -1;
		return { tracks: list, currentIndex: currentIndex >= 0 ? currentIndex : 0 };
	} catch {
		return fallback;
	}
}
