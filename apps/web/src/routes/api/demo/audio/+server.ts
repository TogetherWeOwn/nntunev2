import type { RequestHandler } from './$types';
import { audioCacheHeaders } from '#lib/player/cacheHeaders.js';
import { MEDIA_BYTES, syntheticBody } from '#lib/player/demoMedia.js';

/**
 * Deterministic WAV media for player smoke tests. No provider, no R2, no auth.
 * `?track=<id>` selects distinct samples; byte ranges preserve WAV playback.
 */

function parseRange(header: string, len: number): { start: number; end: number } | undefined {
	const match = /^bytes=([0-9]{0,16})-([0-9]{0,16})$/i.exec(header);
	if (!match) return;
	const [, startText, endText] = match;
	const last = endText === '' ? len - 1 : Number(endText);
	if (!Number.isSafeInteger(last)) return;
	if (startText === '') {
		if (endText === '' || last === 0) return;
		return { start: Math.max(0, len - last), end: len - 1 };
	}
	const start = Number(startText);
	const end = Math.min(last, len - 1);
	if (!Number.isSafeInteger(start) || end < start) return;
	return { start, end };
}

export const GET: RequestHandler = ({ request, url }) => {
	const rangeHeader = request.headers.get('range');
	const range = rangeHeader === null ? undefined : parseRange(rangeHeader, MEDIA_BYTES);
	if (rangeHeader !== null && !range) {
		return new Response(null, { status: 416, headers: {
			'accept-ranges': 'bytes', 'cache-control': 'no-store',
			'content-range': `bytes */${MEDIA_BYTES}`, 'content-length': '0'
		} });
	}
	const tracks = url.searchParams.getAll('track');
	if (tracks.length > 1) return new Response(null, { status: 400, headers: { 'cache-control': 'no-store' } });
	const body = syntheticBody(tracks[0]);
	const headers = new Headers({
		...audioCacheHeaders(),
		'content-type': 'audio/wav',
		'x-content-type-options': 'nosniff',
		'accept-ranges': 'bytes',
		'content-length': String(body.length)
	});

	// No validator is advertised, so an If-Range condition cannot match.
	if (!range || request.headers.has('if-range')) {
		return new Response(body.buffer as ArrayBuffer, { status: 200, headers });
	}
	const slice = body.slice(range.start, range.end + 1);
	headers.set('content-range', `bytes ${range.start}-${range.end}/${body.length}`);
	headers.set('content-length', String(slice.length));
	return new Response(slice.buffer as ArrayBuffer, { status: 206, headers });
};
