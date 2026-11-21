import type { ApiClient } from '../api/client.js';
import { validBinding } from './session-verification.js';

export type TasteSignal = 'dislike' | 'hide' | 'ban' | 'snooze';
export type TrackTaste = { signal: TasteSignal | 'love'; until: number | null };

const PATH = '/v1/me/taste/session';
const MAX_TASTE_ROWS = 500;
export const SNOOZE_DAYS = 30;
const headers = (binding: string) => {
	if (!validBinding(binding)) throw new Error('Missing verified session binding');
	return { 'x-nntune-session-binding': binding };
};

function record(value: unknown): Record<string, unknown> {
	if (!value || typeof value !== 'object' || Array.isArray(value)) throw new Error('Invalid taste response');
	return value as Record<string, unknown>;
}

function preference({ signal, until }: Record<string, unknown>): TrackTaste {
	if (signal === 'snooze' && typeof until === 'number' && Number.isSafeInteger(until) && until > 0) {
		return { signal, until };
	}
	if (until === null && (signal === 'love' || signal === 'dislike' || signal === 'hide' || signal === 'ban')) {
		return { signal, until };
	}
	throw new Error('Invalid taste signal');
}

export async function loadTrackTaste(client: Pick<ApiClient, 'get'>, binding: string) {
	const result = record(await client.get<unknown>(PATH, headers(binding)));
	if (result.session_binding !== binding || !Array.isArray(result.items) || result.items.length > MAX_TASTE_ROWS) {
		throw new Error('Invalid taste list');
	}
	const tracks = new Map<string, TrackTaste>();
	for (const row of result.items) {
		const item = record(row);
		if (item.kind === 'release') continue;
		const id = item.target_id;
		if (item.kind !== 'track' || typeof id !== 'string' || !/^[A-Za-z0-9_-]{1,512}$/.test(id) || tracks.has(id)) {
			throw new Error('Invalid track taste list');
		}
		tracks.set(id, preference(item));
	}
	return { tracks, complete: result.items.length < MAX_TASTE_ROWS };
}

/** Only exact, session-bound receipts can update the Library's confirmed state. */
export async function saveTrackTaste(client: Pick<ApiClient, 'put' | 'delete'>, binding: string, id: string, signal: TasteSignal | null) {
	const target = { kind: 'track', target_id: id };
	const proof = headers(binding);
	const result = record(signal === null
		? await client.delete<unknown>(PATH, proof, target)
		: await client.put<unknown>(PATH, {
			...target, signal, ...(signal === 'snooze' ? { snooze_seconds: SNOOZE_DAYS * 86400 } : {})
		}, proof));
	if (result.session_binding !== binding || result.kind !== target.kind || result.target_id !== id ||
		(signal === null ? typeof result.cleared !== 'boolean' : result.signal !== signal)) {
		throw new Error('Invalid taste confirmation');
	}
	return signal === null ? null : preference(result);
}
