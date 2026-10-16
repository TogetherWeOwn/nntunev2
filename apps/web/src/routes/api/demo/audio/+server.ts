import type { RequestHandler } from './$types';
import { audioCacheHeaders } from '#lib/player/cacheHeaders.js';
import { syntheticBody } from '#lib/player/demoMedia.js';

/**
 * Synthetic media for player smoke tests: deterministic pseudo-audio
 * with `Range`/`206` support. No provider, no R2, no auth. `?track=<id>`
 * selects a distinct body for queue tracks.
 */

type ParsedRange = { start: number; end: number } | { unsatisfiable: true };

function parseRange(header: string | null, len: number): ParsedRange | null {
	if (header === null) return null;
	const match = /^bytes=(\d*)-(\d*)$/.exec(header.trim());
	if (!match) return null;
	const [, startText, endText] = match;
	if (startText === '' && endText === '') return null;
	if (startText === '') {
		const suffix = Number(endText);
		if (!Number.isFinite(suffix)) return null;
		if (suffix <= 0) return { unsatisfiable: true };
		const n = Math.min(suffix, len);
		return { start: len - n, end: len - 1 };
	}
	const start = Number(startText);
	if (!Number.isFinite(start)) return null;
	if (start >= len) return { unsatisfiable: true };
	const end = endText === '' ? len - 1 : Math.min(Number(endText), len - 1);
	if (!Number.isFinite(end)) return null;
	if (end < start) return { unsatisfiable: true };
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
	if ('unsatisfiable' in range) {
		headers.set('content-range', `bytes */${body.length}`);
		headers.set('content-length', '0');
		return new Response(null, { status: 416, headers });
	}
	const slice = body.slice(range.start, range.end + 1);
	headers.set('content-range', `bytes ${range.start}-${range.end}/${body.length}`);
	headers.set('content-length', String(slice.length));
	return new Response(slice.buffer as ArrayBuffer, { status: 206, headers });
};
