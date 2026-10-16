import type { RequestHandler } from './$types';
import { audioCacheHeaders } from '#lib/player/cacheHeaders.js';
import { syntheticBody } from '#lib/player/demoMedia.js';

/**
 * Synthetic media for player smoke tests: deterministic pseudo-audio
 * with `Range`/`206` support. No provider, no R2, no auth. `?track=<id>`
 * selects a distinct body for queue tracks.
 */

function parseRange(header: string | null, len: number): { start: number; end: number } | null {
	if (header === null) return null;
	const match = /^bytes=(\d*)-(\d*)$/.exec(header.trim());
	if (!match) return null;
	const [, startText, endText] = match;
	if (startText === '' && endText === '') return null;
	if (startText === '') {
		const suffix = Number(endText);
		if (!Number.isFinite(suffix) || suffix <= 0) return null;
		const n = Math.min(suffix, len);
		return { start: len - n, end: len - 1 };
	}
	const start = Number(startText);
	if (!Number.isFinite(start) || start >= len) return null;
	const end = endText === '' ? len - 1 : Math.min(Number(endText), len - 1);
	if (!Number.isFinite(end) || end < start) return null;
	return { start, end };
}

export const GET: RequestHandler = ({ request, url }) => {
	const body = syntheticBody(url.searchParams.get('track'));
	const headers = new Headers({
		...audioCacheHeaders(),
		'content-type': 'audio/mpeg',
		'accept-ranges': 'bytes',
		'content-length': String(body.length)
	});

	const range = parseRange(request.headers.get('range'), body.length);
	if (range === null) {
		return new Response(body.buffer as ArrayBuffer, { status: 200, headers });
	}
	const slice = body.slice(range.start, range.end + 1);
	headers.set('content-range', `bytes ${range.start}-${range.end}/${body.length}`);
	headers.set('content-length', String(slice.length));
	return new Response(slice.buffer as ArrayBuffer, { status: 206, headers });
};
