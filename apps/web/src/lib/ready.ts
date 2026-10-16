/**
 * Readiness response builders for `GET /api/ready`.
 *
 * 200 `{ok:true}` while the app can serve traffic; 503 `{ok:false}` with a
 * `Retry-After` header when it cannot, so clients, proxies, and retry
 * middleware never mistake a cold instance for a healthy one. The web
 * frontend is stateless (no bindings yet), so the endpoint reports ready;
 * the 503 builder exists so the first downstream dependency check reuses
 * the pinned shape instead of inventing a new one.
 */

export function readyResponse(): Response {
	return Response.json({ ok: true }, { status: 200 });
}

export function notReadyResponse(retryAfterSeconds: number): Response {
	return Response.json(
		{ ok: false },
		{
			status: 503,
			headers: { 'retry-after': String(Math.max(0, Math.floor(retryAfterSeconds))) }
		}
	);
}
