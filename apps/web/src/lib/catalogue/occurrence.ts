import type { ApiClient } from '../api/client.js';
import {
	isMbid, readRelease,
	type CatalogueFailure, type Release, type Track
} from './catalogue.js';

/**
 * Exact track occurrence: a recording on one release at one medium/position.
 * Canonical URL: /track/<recording>?release=<release>&medium=<n|unknown>&position=<n|unknown>
 * with lowercase MBIDs. "unknown" is the explicit encoding of a null medium/position.
 */
export interface OccurrenceKey {
	recording: string;
	release: string;
	medium: number | null;
	position: number | null;
}

export interface TrackOccurrence { track: Track; release: Release }
export type OccurrenceResult =
	| { value: TrackOccurrence; failure: null }
	| { value: null; failure: CatalogueFailure };
export type OccurrenceLoad = OccurrenceResult & { authGeneration: number };

export const UNKNOWN_OCCURRENCE_PART = 'unknown';

const NOT_FOUND = { value: null, failure: 'not-found' } as const;

function encodePart(value: number | null): string | null {
	if (value === null) return UNKNOWN_OCCURRENCE_PART;
	return Number.isSafeInteger(value) && value >= 0 ? String(value) : null;
}

function decodePart(values: string[]): { ok: true; value: number | null } | { ok: false } {
	if (values.length !== 1) return { ok: false };
	const text = values[0];
	if (text === UNKNOWN_OCCURRENCE_PART) return { ok: true, value: null };
	if (!/^(0|[1-9][0-9]*)$/.test(text)) return { ok: false };
	const value = Number(text);
	return Number.isSafeInteger(value) ? { ok: true, value } : { ok: false };
}

/** Returns null when any part is invalid, so callers never emit an ambiguous link. */
export function occurrenceHref(key: OccurrenceKey): string | null {
	const medium = encodePart(key.medium);
	const position = encodePart(key.position);
	if (!isMbid(key.recording) || !isMbid(key.release) || medium === null || position === null) return null;
	return `/track/${key.recording.toLowerCase()}?release=${key.release.toLowerCase()}&medium=${medium}&position=${position}`;
}

/** Link for a track row as listed on its own release. */
export function trackOccurrenceHref(track: Track): string | null {
	return occurrenceHref({
		recording: track.mbid, release: track.release_mbid,
		medium: track.medium_no, position: track.track_no
	});
}

/** Omit ambiguous occurrences rather than offering a destination that cannot resolve. */
export function releaseOccurrenceLinks(tracks: Track[]): Map<Track, string> {
	const counts = new Map<string, number>();
	for (const track of tracks) {
		const href = trackOccurrenceHref(track);
		if (href) counts.set(href, (counts.get(href) ?? 0) + 1);
	}
	const links = new Map<Track, string>();
	for (const track of tracks) {
		const href = trackOccurrenceHref(track);
		if (href && counts.get(href) === 1) links.set(track, href);
	}
	return links;
}

/** Validates every route and query input; any malformed or repeated input yields null. */
export function parseOccurrence(recording: unknown, search: URLSearchParams | string): OccurrenceKey | null {
	if (!isMbid(recording)) return null;
	const params = typeof search === 'string' ? new URLSearchParams(search) : search;
	const releases = params.getAll('release');
	if (releases.length !== 1 || !isMbid(releases[0])) return null;
	const medium = decodePart(params.getAll('medium'));
	const position = decodePart(params.getAll('position'));
	if (!medium.ok || !position.ok) return null;
	return { recording: recording.toLowerCase(), release: releases[0].toLowerCase(), medium: medium.value, position: position.value };
}

/** Pure selector: exactly one match or null (missing, foreign release or ambiguous duplicates). */
export function selectOccurrence(tracks: Track[], key: OccurrenceKey): Track | null {
	const matches = tracks.filter((track) => track.mbid.toLowerCase() === key.recording
		&& track.release_mbid.toLowerCase() === key.release
		&& track.medium_no === key.medium && track.track_no === key.position);
	return matches.length === 1 ? matches[0] : null;
}

/**
 * One bounded release read (no library listing, no playback inference). The generation
 * is sampled before the request and rechecked after; a changed generation discards the
 * response as signed-out so an old account's data is never shown.
 */
export async function readOccurrence(
	client: ApiClient,
	recording: unknown,
	search: URLSearchParams | string,
	generation: () => number
): Promise<OccurrenceLoad> {
	const authGeneration = generation();
	const key = parseOccurrence(recording, search);
	if (!key) return { ...NOT_FOUND, authGeneration };
	const result = await readRelease(client, key.release);
	if (authGeneration !== generation()) return { value: null, failure: 'signed-out', authGeneration };
	if (!result.value) return { value: null, failure: result.failure, authGeneration };
	if (result.value.release.mbid.toLowerCase() !== key.release) return { value: null, failure: 'unavailable', authGeneration };
	const track = selectOccurrence(result.value.tracks, key);
	return track
		? { value: { track, release: result.value.release }, failure: null, authGeneration }
		: { ...NOT_FOUND, authGeneration };
}
