import { ApiError, type ApiClient } from '../api/client.js';

type LikesClient = Pick<ApiClient, 'get' | 'put' | 'delete' | 'post'>;

export interface LibraryLike {
	kind: 'track' | 'release';
	targetId: string;
	createdAt: number;
}

const LIKES_PATH = '/v1/me/likes';
const MAX_LIKES = 500;
// Import does catalogue resolution and a preference write per item. Keep each
// request comfortably below the API's 500-item ceiling.
const IMPORT_BATCH_SIZE = 50;
const REJECTION_ID_LIMIT = 64;
const TRACK_ID = /^[A-Za-z0-9_-]{1,512}$/;
const RELEASE_ID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;

function isTrackId(value: unknown): value is string {
	return typeof value === 'string' && TRACK_ID.test(value);
}

function isCount(value: unknown): value is number {
	return typeof value === 'number' && Number.isSafeInteger(value) && value >= 0;
}

function record(value: unknown): Record<string, unknown> {
	return (value ?? {}) as Record<string, unknown>;
}

export async function isLikesSession(client: Pick<LikesClient, 'get'>, binding: string): Promise<boolean> {
	try {
		return record(await client.get<unknown>(`${LIKES_PATH}/session`)).session_binding === binding;
	} catch (error) {
		if (error instanceof ApiError && error.status === 401) return false;
		throw error;
	}
}

export async function loadLikes(client: Pick<LikesClient, 'get'>) {
	const { items, session_binding: binding } = record(await client.get<unknown>(LIKES_PATH));
	if (!Array.isArray(items) || items.length > MAX_LIKES || typeof binding !== 'string' || !binding) {
		throw new Error('Invalid likes list');
	}
	const seen = new Set<string>();
	const likes = items.map((item: unknown): LibraryLike => {
		const { kind, target_id, created_at } = record(item);
		if ((kind !== 'track' && kind !== 'release') || !isTrackId(target_id) ||
			(kind === 'release' && !RELEASE_ID.test(target_id)) || !isCount(created_at)) {
			throw new Error('Invalid likes list');
		}
		const key = `${kind}:${target_id}`;
		if (seen.has(key)) throw new Error('Invalid likes list');
		seen.add(key);
		return { kind, targetId: target_id, createdAt: created_at };
	});
	return { items: likes, binding };
}

/** Return the confirmed server identity, which can differ from the input MBID. */
export async function setTrackLike(
	client: Pick<LikesClient, 'put' | 'delete'>,
	id: string,
	liked: boolean
): Promise<string> {
	if (!isTrackId(id)) throw new Error('Invalid track ID');
	// The server checks exact member job IDs before resolving recording MBIDs.
	const path = `${LIKES_PATH}/track/${id}`;
	const receipt = record(liked ? await client.put<unknown>(path, {}) : await client.delete<unknown>(path));
	if (!isTrackId(receipt.target_id) ||
		(receipt.target_id !== id && (!RELEASE_ID.test(receipt.target_id) || receipt.target_id !== id.toLowerCase())) ||
		(liked
			? !isCount(receipt.liked_at) || receipt.liked !== undefined
			: receipt.liked !== false || receipt.liked_at !== undefined)) {
		throw new Error('Invalid like confirmation');
	}
	return receipt.target_id;
}

function confirmedIds(batch: readonly string[], payload: unknown): string[] {
	const { added, already, rejected } = record(payload);
	if (!isCount(added) || !isCount(already) || !Array.isArray(rejected) ||
		added + already + rejected.length !== batch.length) {
		throw new Error('Invalid likes import confirmation');
	}
	const candidates = new Map<string, number>();
	for (const id of batch) {
		const prefix = id.slice(0, REJECTION_ID_LIMIT);
		candidates.set(prefix, (candidates.get(prefix) ?? 0) + 1);
	}
	const rejectedPrefixes = new Set<string>();
	for (const item of rejected) {
		const { target_id, reason } = record(item);
		const remaining = typeof target_id === 'string' ? candidates.get(target_id) : undefined;
		if (typeof target_id !== 'string' || !remaining || (reason !== 'invalid' && reason !== 'not_member')) {
			throw new Error('Invalid likes import confirmation');
		}
		candidates.set(target_id, remaining - 1);
		rejectedPrefixes.add(target_id);
	}
	// Rejections echo at most 64 characters. If several IDs share that prefix,
	// keep all of them rather than deleting an unconfirmed local favorite.
	return batch.filter((id) => !rejectedPrefixes.has(id.slice(0, REJECTION_ID_LIMIT)));
}

/**
 * Import a snapshot of legacy track IDs. The caller binds the client and
 * retirement callback to the same auth scope; never use an abandoned receipt
 * to update device storage. Previously confirmed batches survive a later error.
 */
export async function importDeviceLikes(
	client: Pick<LikesClient, 'post'>,
	ids: readonly string[],
	scope: {
		binding: string;
		isCurrent: () => boolean;
		retireConfirmed: (ids: readonly string[]) => Promise<boolean>;
	}
): Promise<boolean> {
	const targets = [...new Set(ids.filter(isTrackId))];
	for (let start = 0; start < targets.length; start += IMPORT_BATCH_SIZE) {
		if (!scope.isCurrent()) return false;
		const batch = targets.slice(start, start + IMPORT_BATCH_SIZE);
		const receipt = await client.post<unknown>(`${LIKES_PATH}/import`, {
			session_binding: scope.binding,
			items: batch.map((target_id) => ({ kind: 'track', target_id }))
		});
		if (!scope.isCurrent()) return false;
		const confirmed = confirmedIds(batch, receipt);
		if (confirmed.length && !(await scope.retireConfirmed(confirmed))) return false;
	}
	return true;
}
