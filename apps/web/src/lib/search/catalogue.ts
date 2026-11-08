/**
 * Catalogue search client for `GET /v1/mb/search`.
 *
 * Non-visual: fans out one request per kind, validates every hit before it
 * can become a link, and fails the whole search if any request fails so the
 * caller can show its library-only fallback instead of a partial catalogue.
 *
 * Guarantees:
 * - One deadline covers fetch AND body read for all three requests.
 * - A caller abort or the deadline settles the promise immediately, even if
 *   the transport ignores the signal; a late response is never published.
 * - Errors carry fixed generic messages, never remote copy.
 */

import { readCappedJson } from '#lib/api/cappedJson.js';

export type SearchKind = 'artist' | 'release' | 'recording';

export interface CatalogueHit {
	mbid: string;
	label: string;
	score: number;
	kind: SearchKind;
	artistMbid?: string;
	artistName?: string;
	availability: 'available';
	destination: string | null;
	occurrences: { id: string; occurrence_key: string }[];
}

export type CatalogueSearchErrorCode = 'aborted' | 'timeout' | 'failed';

const MESSAGES: Record<CatalogueSearchErrorCode, string> = {
	aborted: 'Catalogue search was cancelled.',
	timeout: 'Catalogue search timed out.',
	failed: 'Catalogue search failed.'
};

/** Fixed-message failure; never wraps remote text. */
export class CatalogueSearchError extends Error {
	readonly code: CatalogueSearchErrorCode;
	constructor(code: CatalogueSearchErrorCode) {
		super(MESSAGES[code]);
		this.name = 'CatalogueSearchError';
		this.code = code;
	}
}

export const CATALOGUE_KINDS: readonly SearchKind[] = ['artist', 'release', 'recording'];
export const CATALOGUE_LIMIT = 12;
export const CATALOGUE_TIMEOUT_MS = 8000;
export const CATALOGUE_QUERY_MIN = 2;
export const CATALOGUE_QUERY_MAX = 100;
const MAX_LABEL_CHARS = 300;

const UUID_RE = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
// C0/C1 controls plus Unicode line and paragraph separators.
const CONTROL_RE = /[\p{Cc}\u2028\u2029]/u;

/** Trimmed query if it is 2-100 code points with no control chars, else null. */
function normaliseQuery(query: string): string | null {
	if (typeof query !== 'string') return null;
	const q = query.trim();
	if (CONTROL_RE.test(q)) return null;
	const length = Array.from(q).length;
	if (length < CATALOGUE_QUERY_MIN || length > CATALOGUE_QUERY_MAX) return null;
	return q;
}

function isRecord(value: unknown): value is Record<string, unknown> {
	return typeof value === 'object' && value !== null && !Array.isArray(value);
}

function cleanText(value: unknown): string | null {
	if (typeof value !== 'string') return null;
	const text = value.trim();
	if (text.length === 0 || CONTROL_RE.test(text)) return null;
	return Array.from(text).slice(0, MAX_LABEL_CHARS).join('');
}

function parseHit(raw: unknown, kind: SearchKind): CatalogueHit | null {
	if (!isRecord(raw)) return null;
	const mbid = raw.mbid;
	if (typeof mbid !== 'string' || !UUID_RE.test(mbid)) return null;
	const label = cleanText(kind === 'artist' ? raw.name : raw.title);
	if (label === null) return null;
	const score = raw.score;
	if (typeof score !== 'number' || !Number.isFinite(score)) return null;
	// Raw metadata is not the application search contract. Treat missing or
	// unknown readiness as service failure, not as a successful empty search.
	if (raw.availability !== 'available' || !Array.isArray(raw.occurrences)) {
		throw new CatalogueSearchError('failed');
	}
	const normalizedMbid = mbid.toLowerCase();
	const expectedDestination = kind === 'artist' ? `/artist/${normalizedMbid}`
		: kind === 'release' ? `/album/${normalizedMbid}` : null;
	if (raw.destination !== null && raw.destination !== expectedDestination) {
		throw new CatalogueSearchError('failed');
	}
	if (raw.occurrences.length > 50) throw new CatalogueSearchError('failed');
	const occurrences = raw.occurrences.map((value: unknown) => {
		if (!isRecord(value) || cleanText(value.id) !== value.id || cleanText(value.occurrence_key) !== value.occurrence_key
			|| typeof value.id !== 'string' || typeof value.occurrence_key !== 'string') {
			throw new CatalogueSearchError('failed');
		}
		return { id: value.id, occurrence_key: value.occurrence_key };
	});
	if ((kind === 'recording' && occurrences.length === 0) || (kind !== 'recording' && occurrences.length > 0)) {
		throw new CatalogueSearchError('failed');
	}
	const hit: CatalogueHit = { mbid: normalizedMbid, label, score, kind,
		availability: 'available', destination: raw.destination as string | null, occurrences };
	if (kind !== 'artist') {
		const artistMbid = raw.artist_mbid;
		const artistName = cleanText(raw.artist_name);
		if (typeof artistMbid === 'string' && UUID_RE.test(artistMbid)) {
			hit.artistMbid = artistMbid.toLowerCase();
		}
		if (artistName !== null) hit.artistName = artistName;
	}
	return hit;
}

/** Throws on a malformed envelope; drops individual malformed hits. */
function parseResponse(body: unknown, kind: SearchKind): CatalogueHit[] {
	if (!isRecord(body) || body.type !== kind || !Array.isArray(body.results)) {
		throw new CatalogueSearchError('failed');
	}
	const hits: CatalogueHit[] = [];
	for (const raw of body.results.slice(0, CATALOGUE_LIMIT)) {
		const hit = parseHit(raw, kind);
		if (hit) hits.push(hit);
	}
	return hits;
}

function mergeHits(groups: CatalogueHit[][]): CatalogueHit[] {
	const best = new Map<string, CatalogueHit>();
	for (const hit of groups.flat()) {
		const key = `${hit.kind}:${hit.mbid}`;
		const prev = best.get(key);
		if (!prev || hit.score > prev.score) best.set(key, hit);
	}
	const order = (k: SearchKind) => CATALOGUE_KINDS.indexOf(k);
	return [...best.values()].sort(
		(a, b) =>
			b.score - a.score ||
			order(a.kind) - order(b.kind) ||
			(a.mbid < b.mbid ? -1 : a.mbid > b.mbid ? 1 : 0)
	);
}

async function fetchKind(
	url: string,
	kind: SearchKind,
	signal: AbortSignal,
	fetchFn: typeof fetch
): Promise<CatalogueHit[]> {
	const response = await fetchFn(url, {
		method: 'GET',
		credentials: 'include',
		headers: { accept: 'application/json' },
		signal
	});
	if (signal.aborted) {
		void response.body?.cancel().catch(() => undefined);
		throw new CatalogueSearchError('aborted');
	}
	if (!response.ok) {
		try {
			await response.body?.cancel();
		} catch {
			// ignore
		}
		throw new CatalogueSearchError('failed');
	}
	return parseResponse(await readCappedJson(response), kind);
}

/**
 * Search the catalogue for artists, releases and recordings in parallel.
 * Returns [] without a request for an invalid query. Rejects with
 * {@link CatalogueSearchError} on any failure, timeout or caller abort.
 */
export async function searchCatalogue(
	baseUrl: string,
	query: string,
	signal: AbortSignal,
	fetchFn: typeof fetch = fetch
): Promise<CatalogueHit[]> {
	const q = normaliseQuery(query);
	if (q === null) return [];
	if (signal.aborted) throw new CatalogueSearchError('aborted');

	const base = baseUrl.replace(/\/+$/, '');
	const encoded = encodeURIComponent(q);
	const controller = new AbortController();

	let stop!: (code: CatalogueSearchErrorCode) => void;
	const gate = new Promise<never>((_, reject) => {
		stop = (code) => {
			controller.abort();
			reject(new CatalogueSearchError(code));
		};
	});
	gate.catch(() => undefined);

	const onAbort = () => stop('aborted');
	signal.addEventListener('abort', onAbort, { once: true });
	const timer = setTimeout(() => stop('timeout'), CATALOGUE_TIMEOUT_MS);

	try {
		const work = Promise.all(
			CATALOGUE_KINDS.map((kind) =>
				fetchKind(
					`${base}/v1/mb/search?q=${encoded}&type=${kind}&limit=${CATALOGUE_LIMIT}&offset=0`,
					kind,
					controller.signal,
					fetchFn
				)
			)
		);
		work.catch(() => undefined);
		const groups = await Promise.race([work, gate]);
		if (signal.aborted) throw new CatalogueSearchError('aborted');
		return mergeHits(groups);
	} catch (error) {
		// Cancel any sibling request still in flight.
		controller.abort();
		if (error instanceof CatalogueSearchError) throw error;
		throw new CatalogueSearchError(signal.aborted ? 'aborted' : 'failed');
	} finally {
		clearTimeout(timer);
		signal.removeEventListener('abort', onAbort);
	}
}
