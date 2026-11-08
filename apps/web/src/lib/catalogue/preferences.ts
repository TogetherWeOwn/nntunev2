import { ApiClient, ApiError } from '../api/client.js';
import { isMbid } from './catalogue.js';

export type PreferenceKind = 'albums' | 'artists';
export type PreferenceFailure = 'signed-out' | 'not-found' | 'account-deleting' | 'unavailable';
export type PreferenceResult = { value: boolean; failure: null } | { value: null; failure: PreferenceFailure };

function selected(value: unknown, kind: PreferenceKind): boolean | null {
	if (typeof value !== 'object' || value === null || Array.isArray(value)) return null;
	const row = value as Record<string, unknown>;
	const enabled = row[kind === 'albums' ? 'saved' : 'followed'];
	const at = row[kind === 'albums' ? 'savedAt' : 'followedAt'];
	if (enabled === false && at === null) return false;
	if (enabled !== true || typeof at !== 'string' || !/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z$/.test(at)) return null;
	const date = new Date(at);
	return Number.isFinite(date.getTime()) && date.toISOString().replace('.000Z', 'Z') === at ? true : null;
}

/** Undefined means read; false means remove. No collection-list inference or local persistence. */
export async function collectionPreference(
	client: ApiClient, kind: PreferenceKind, mbid: string, enabled?: boolean
): Promise<PreferenceResult> {
	if (!isMbid(mbid)) return { value: null, failure: 'not-found' };
	const path = `/v1/me/library/${kind}/${mbid.toLowerCase()}`;
	try {
		const body = enabled === undefined ? await client.get<unknown>(path)
			: enabled ? await client.put<unknown>(path, undefined) : await client.delete<unknown>(path);
		const value = selected(body, kind);
		return value === null || (enabled !== undefined && value !== enabled)
			? { value: null, failure: 'unavailable' } : { value, failure: null };
	} catch (error) {
		return { value: null, failure: error instanceof ApiError && error.status === 401 ? 'signed-out'
			: error instanceof ApiError && error.status === 404 ? 'not-found'
			: error instanceof ApiError && error.status === 409 && error.code === 'account_deleting' ? 'account-deleting'
			: 'unavailable' };
	}
}
