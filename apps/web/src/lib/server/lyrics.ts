const MAX_LRC_LENGTH = 64 * 1024; // UTF-16 code units
const MAX_CUES = 1024;
const CACHE_CAPACITY = 128;
const HIT_TTL_MS = 60 * 60_000;
const MISS_TTL_MS = 5 * 60_000;

export type LyricCue = Readonly<{ atMs: number; text: string }>;
export type TimedLyrics = readonly LyricCue[];

/** Empty timed rows clear the displayed line; a document with only clears is absent. */
export function parseTimedLrc(input?: string | null): TimedLyrics | null {
	if (typeof input !== 'string' || input.length > MAX_LRC_LENGTH) return null;
	const cues: LyricCue[] = [];
	const stamp = /\[(\d{1,3}):([0-5]\d)(?:\.(\d{1,3}))?\]/y;
	let offsetMs = 0;
	for (const row of input.split(/\r\n?|\n/)) {
		const line = row.trim();
		const offset = /^\[offset:([+-]?\d{1,6})\]$/i.exec(line);
		if (offset) {
			offsetMs = Number(offset[1]);
			continue;
		}
		const times: number[] = [];
		let end = 0;
		stamp.lastIndex = 0;
		for (let match; (match = stamp.exec(line)); ) {
			if (cues.length + times.length >= MAX_CUES) return null;
			const [, minutes, seconds, fraction = ''] = match;
			times.push(Number(minutes) * 60_000 + Number(seconds) * 1000 + Number(fraction.padEnd(3, '0')));
			end = stamp.lastIndex;
		}
		if (!end || /^\[\d+:/.test(line.slice(end))) continue;
		const text = line.slice(end).trimStart();
		for (const atMs of times) cues.push({ atMs, text });
	}
	if (!cues.some((cue) => cue.text !== '')) return null;
	// A positive LRC offset advances display. Preserve chronology even if several
	// cues then collapse onto zero; ties at the same original time keep file order.
	cues.sort((a, b) => a.atMs - b.atMs);
	return Object.freeze(cues.map(({ atMs, text }) => Object.freeze({
		atMs: Math.max(0, atMs - offsetMs), text
	})));
}

/** Last cue at or before the playback position, or null before the first cue. */
export function cueAt(cues: TimedLyrics, positionMs: number): LyricCue | null {
	if (!Number.isFinite(positionMs) || positionMs < 0) return null;
	let low = 0;
	let high = cues.length;
	while (low < high) {
		const mid = Math.floor((low + high) / 2);
		if (cues[mid].atMs <= positionMs) low = mid + 1;
		else high = mid;
	}
	return cues[low - 1] ?? null;
}

export type ExactTrack = Readonly<{
	trackName: string;
	artistName: string;
	albumName: string;
	durationSeconds: number;
}>;

type ProviderReply =
	| { kind: 'match'; track: ExactTrack; instrumental: boolean; syncedLyrics: string | null }
	| { kind: 'not-found' }
	| { kind: 'unavailable' };

/** Future adapter contract: LRCLIB /api/get-cached only, never search. Report
 * not-found only for a confirmed track miss; transport/decoding failures are unavailable.
 */
export type ExactLyricsProvider = (track: ExactTrack) => Promise<ProviderReply>;

type LocalLyrics = {
	embedded?: string | null;
	instrumental?: boolean;
	// Already-authorized paths in the same library namespace; this module never reads files.
	mediaPath?: string;
	sidecar?: { path: string; lrc: string };
};

export type ResolvedLyrics = Readonly<{ cues: TimedLyrics } & (
	| { source: 'embedded' | 'sidecar' }
	| { source: 'lrclib'; attributionId: 'lrclib' }
)>;

type CacheEntry = { value: ResolvedLyrics | null; savedAt: number; expiresAt: number };

function isFresh(entry: CacheEntry, now: number): boolean {
	return entry.savedAt <= now && now < entry.expiresAt;
}

function parseSidecar({ mediaPath, sidecar }: LocalLyrics): TimedLyrics | null {
	if (!mediaPath || !sidecar) return null;
	const dot = mediaPath.lastIndexOf('.');
	const stem = dot > mediaPath.lastIndexOf('/') + 1 ? mediaPath.slice(0, dot) : mediaPath;
	const expected = `${stem}.lrc`;
	return expected !== mediaPath && sidecar.path === expected ? parseTimedLrc(sidecar.lrc) : null;
}

function validDuration(seconds: number): boolean {
	return Number.isFinite(seconds) && seconds > 0 && seconds <= 86_400;
}

function exactMatch(track: ExactTrack, reply: ExactTrack): boolean {
	return track.trackName === reply.trackName && track.artistName === reply.artistName &&
		track.albumName === reply.albumName && validDuration(reply.durationSeconds) &&
		// LRCLIB exact lookup allows a two-second difference in track length.
		Math.abs(track.durationSeconds - reply.durationSeconds) <= 2;
}

/** One in-memory cache per library. No provider is installed by default. */
export function createLyricsResolver(provider?: ExactLyricsProvider, now = Date.now) {
	const cache = new Map<string, CacheEntry>();
	return async function resolve(local: LocalLyrics, track?: ExactTrack): Promise<ResolvedLyrics | null> {
		if (local.instrumental) return null;
		const embedded = parseTimedLrc(local.embedded);
		if (embedded) return { source: 'embedded', cues: embedded };
		const sidecar = parseSidecar(local);
		if (sidecar) return { source: 'sidecar', cues: sidecar };
		if (!provider || !track) return null;

		// Project only the exact lookup fields; never pass file paths or library IDs.
		const { trackName, artistName, albumName, durationSeconds } = track;
		const names = [trackName, artistName, albumName];
		if (names.some((name) => typeof name !== 'string' || name.length > 256 || !name.trim()) ||
			!validDuration(durationSeconds)) return null;
		const query = Object.freeze({ trackName, artistName, albumName, durationSeconds });
		const key = JSON.stringify([...names, durationSeconds]);
		const cutoff = now();
		for (const [id, entry] of cache) {
			if (!isFresh(entry, cutoff)) cache.delete(id);
		}
		const cached = cache.get(key);
		if (cached) return cached.value;

		let reply: ProviderReply;
		try {
			reply = await provider(query);
		} catch {
			return null;
		}
		let value: ResolvedLyrics | null = null;
		if (reply?.kind === 'match') {
			if (!reply.track || !exactMatch(query, reply.track) || typeof reply.instrumental !== 'boolean') return null;
			if (!reply.instrumental && reply.syncedLyrics !== null) {
				const cues = parseTimedLrc(reply.syncedLyrics);
				if (!cues) return null; // Bad payload is not a confirmed miss.
				value = Object.freeze({ source: 'lrclib', attributionId: 'lrclib', cues });
			}
		} else if (reply?.kind !== 'not-found') {
			return null;
		}
		const savedAt = now();
		// Another lookup may have filled this key while the provider was pending.
		// A confirmed miss must not overwrite a still-valid hit.
		const current = cache.get(key);
		if (!value && current?.value && isFresh(current, savedAt)) return current.value;
		cache.delete(key);
		if (cache.size >= CACHE_CAPACITY) {
			const oldest = cache.keys().next();
			if (!oldest.done) cache.delete(oldest.value);
		}
		cache.set(key, { value, savedAt, expiresAt: savedAt + (value ? HIT_TTL_MS : MISS_TTL_MS) });
		return value;
	};
}
