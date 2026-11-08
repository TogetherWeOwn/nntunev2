import { ApiClient, ApiError } from '../api/client.js';
import { formatDuration } from '../format.js';

export interface Artist {
	mbid: string;
	name: string;
	sort_name: string;
	type: string | null;
	country: string | null;
}

export interface Release {
	mbid: string;
	title: string;
	release_type: string | null;
	year: number | null;
	artist_mbid: string | null;
	artist: string | null;
	art_url: string | null;
	release_group_mbid?: string | null;
}

export interface Track {
	mbid: string;
	title: string;
	release_mbid: string;
	release: string;
	artist_mbid: string | null;
	artist: string | null;
	duration_ms: number | null;
	track_no: number | null;
	medium_no: number | null;
	art_url: string | null;
}

export interface ReleaseDetail { release: Release; tracks: Track[] }
export interface ArtistDetail { artist: Artist; releases: Release[]; truncated: boolean }
export type CatalogueFailure = 'signed-out' | 'not-found' | 'unavailable';
export type CatalogueResult<T> = { value: T; failure: null } | { value: null; failure: CatalogueFailure };

export function isMbid(value: unknown): value is string {
	return typeof value === 'string' && /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(value);
}

function record(value: unknown): value is Record<string, unknown> {
	return typeof value === 'object' && value !== null && !Array.isArray(value);
}
const nullableText = (value: unknown) => value === null || typeof value === 'string';
const nullableMbid = (value: unknown) => value === null || isMbid(value);
const nullableInteger = (value: unknown) => value === null || (Number.isSafeInteger(value) && (value as number) >= 0);

function isRelease(value: unknown): value is Release {
	return record(value) && isMbid(value.mbid) && typeof value.title === 'string'
		&& nullableText(value.release_type) && nullableInteger(value.year)
		&& nullableMbid(value.artist_mbid) && nullableText(value.artist) && nullableText(value.art_url)
		&& (value.release_group_mbid === undefined || nullableMbid(value.release_group_mbid));
}

function isTrack(value: unknown): value is Track {
	return record(value) && isMbid(value.mbid) && typeof value.title === 'string'
		&& isMbid(value.release_mbid) && typeof value.release === 'string'
		&& nullableMbid(value.artist_mbid) && nullableText(value.artist)
		&& nullableInteger(value.duration_ms) && nullableInteger(value.track_no)
		&& nullableInteger(value.medium_no) && nullableText(value.art_url);
}

export function isReleaseDetail(value: unknown): value is ReleaseDetail {
	if (!record(value) || !isRelease(value.release) || !Array.isArray(value.tracks) || value.tracks.length > 1000) return false;
	const id = value.release.mbid.toLowerCase();
	return value.tracks.every((track) => isTrack(track) && track.release_mbid.toLowerCase() === id);
}

export function isArtistDetail(value: unknown): value is ArtistDetail {
	if (!record(value) || !record(value.artist) || !isMbid(value.artist.mbid)
		|| typeof value.artist.name !== 'string' || typeof value.artist.sort_name !== 'string'
		|| !nullableText(value.artist.type) || !nullableText(value.artist.country)
		|| !Array.isArray(value.releases) || value.releases.length > 50 || typeof value.truncated !== 'boolean') return false;
	const id = value.artist.mbid.toLowerCase();
	return value.releases.every((release) => isRelease(release) && release.artist_mbid?.toLowerCase() === id);
}

async function read<T>(client: ApiClient, path: string, id: string, validate: (value: unknown) => value is T): Promise<CatalogueResult<T>> {
	if (!isMbid(id)) return { value: null, failure: 'not-found' };
	try {
		const value = await client.get<unknown>(`${path}/${id.toLowerCase()}`);
		if (!validate(value)) return { value: null, failure: 'unavailable' };
		return { value, failure: null };
	} catch (error) {
		return { value: null, failure: error instanceof ApiError && error.status === 401 ? 'signed-out'
			: error instanceof ApiError && error.status === 404 ? 'not-found' : 'unavailable' };
	}
}

export async function readRelease(client: ApiClient, id: string): Promise<CatalogueResult<ReleaseDetail>> {
	const result = await read(client, '/v1/releases', id, isReleaseDetail);
	return result.value && result.value.release.mbid.toLowerCase() !== id.toLowerCase()
		? { value: null, failure: 'unavailable' } : result;
}

export async function readArtist(client: ApiClient, id: string): Promise<CatalogueResult<ArtistDetail>> {
	const result = await read(client, '/v1/artists', id, isArtistDetail);
	return result.value && result.value.artist.mbid.toLowerCase() !== id.toLowerCase()
		? { value: null, failure: 'unavailable' } : result;
}

/** Only the reserved own-API serving path is allowed; mirror URLs never reach images. */
export function releaseArtwork(release: Release, baseUrl: string): string | null {
	const path = `/v1/releases/${release.mbid.toLowerCase()}/art`;
	if (release.art_url?.toLowerCase() !== path) return null;
	return `${baseUrl.replace(/\/$/, '')}${path}`;
}

export function trackDuration(ms: number | null): string {
	return ms === null ? 'Unknown' : formatDuration(ms / 1000);
}

export function releaseDuration(tracks: Track[]): string | null {
	return tracks.length === 0 || tracks.some((track) => track.duration_ms === null)
		? null : formatDuration(tracks.reduce((sum, track) => sum + track.duration_ms!, 0) / 1000);
}

export function discTracks(tracks: Track[]): { medium: number | null; tracks: Track[] }[] {
	const groups = new Map<number | null, Track[]>();
	for (const track of tracks) {
		const group = groups.get(track.medium_no) ?? [];
		group.push(track);
		groups.set(track.medium_no, group);
	}
	return [...groups].sort(([a], [b]) => (a ?? Infinity) - (b ?? Infinity))
		.map(([medium, rows]) => ({ medium, tracks: [...rows].sort((a, b) => (a.track_no ?? Infinity) - (b.track_no ?? Infinity)) }));
}

export type ReleaseFilter = 'all' | 'albums' | 'singles' | 'compilations';
export const releaseFilters: { value: ReleaseFilter; label: string }[] = [
	{ value: 'all', label: 'All' }, { value: 'albums', label: 'Albums' },
	{ value: 'singles', label: 'Singles & EPs' }, { value: 'compilations', label: 'Compilations' }
];

export function filterReleases(releases: Release[], filter: ReleaseFilter): Release[] {
	return releases.filter((release) => {
		const type = release.release_type?.toLowerCase() ?? '';
		if (filter === 'all') return true;
		if (filter === 'compilations') return type.includes('compilation');
		if (type.includes('compilation')) return false;
		return filter === 'albums' ? type === 'album' : type === 'single' || type === 'ep';
	});
}

/** Missing release-group IDs remain separate: similar titles are not identity. */
export function groupReleases(releases: Release[]): Release[][] {
	const groups = new Map<string, Release[]>();
	for (const release of releases) {
		const key = release.release_group_mbid?.toLowerCase() ?? release.mbid.toLowerCase();
		const group = groups.get(key) ?? [];
		group.push(release);
		groups.set(key, group);
	}
	return [...groups.values()];
}
