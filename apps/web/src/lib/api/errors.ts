/**
 * Shared parsing for the workers' structured error envelope
 * (`{code, message, retryable}`, registered in `docs/api-errors.md`),
 * plus the one client-local code below that the web clients emit
 * themselves and never receive from a worker.
 *
 * Unknown codes fall back to the documented generic (`request_failed`
 * client-side) while keeping the server `message` for display. Legacy
 * `{"error": "..."}` bodies are still accepted during migration: the code
 * is then derived from the HTTP status.
 */

/** Every code in the `docs/api-errors.md` registry. */
export const KNOWN_ERROR_CODES = new Set([
	'unauthorized',
	'reauth_required',
	'provider_auth_expired',
	'plan_limit_exceeded',
	'subscription_required',
	'forbidden',
	'not_found',
	'conflict',
	'account_deleting',
	'invalid_request',
	'MB_SEARCH_EMPTY_QUERY',
	'payload_too_large',
	'range_not_satisfiable',
	'validation_failed',
	'rate_limited',
	'internal_error',
	'corrupt_content',
	'upstream_unavailable',
	'upstream_circuit_open',
	'transport_port_not_allowed',
	'service_unavailable'
]);

/**
 * Client-local codes the web app emits itself and never receives from a
 * worker: `request_timeout` from `ApiClient.timeoutError()` (and the same
 * code from `billingClient.requestJson`'s deadline) at status 504,
 * retryable, never a paywall or auth classification.
 *
 * Intentionally NOT in `docs/api-errors.md` nor `KNOWN_ERROR_CODES`:
 * no worker emits it, so there is no owning worker and no `Adding a
 * code` worker-registry entry to keep in drift. `parseErrorBody`
 * round-trips these codes with the same retryable rule as registered
 * codes (explicit boolean wins, otherwise the status policy, which is
 * retryable at 504); every other code keeps its existing behavior.
 */
export const CLIENT_LOCAL_ERROR_CODES = new Set(['request_timeout']);

export interface ParsedApiError {
	code: string;
	message: string;
	retryable: boolean;
}

/** Code derived from status alone for legacy bodies without a code. */
export function codeFromStatus(status: number): string {
	if (status === 400) return 'invalid_request';
	if (status === 401) return 'unauthorized';
	if (status === 402) return 'plan_limit_exceeded';
	if (status === 403) return 'forbidden';
	if (status === 404) return 'not_found';
	if (status === 409) return 'conflict';
	if (status === 413) return 'payload_too_large';
	if (status === 416) return 'range_not_satisfiable';
	if (status === 422) return 'validation_failed';
	if (status === 429) return 'rate_limited';
	if (status === 503) return 'service_unavailable';
	return 'request_failed';
}

/** Retry policy derived from status when the body carries none. */
export function retryableFromStatus(status: number): boolean {
	return status === 429 || status >= 500;
}

/** Parse an already-decoded error body for `status`. */
export function parseErrorBody(body: unknown, status: number): ParsedApiError {
	const fallback: ParsedApiError = {
		code: codeFromStatus(status),
		message: `request failed with status ${status}`,
		retryable: retryableFromStatus(status)
	};
	if (!body || typeof body !== 'object' || Array.isArray(body)) return fallback;
	const record = body as { code?: unknown; message?: unknown; retryable?: unknown; error?: unknown };
	const detail = Object.hasOwn(record, 'message') ? record.message : undefined;
	const legacy = Object.hasOwn(record, 'error') ? record.error : undefined;
	const message =
		typeof detail === 'string' && detail.length > 0
			? detail
			: typeof legacy === 'string' && legacy.length > 0
				? legacy
				: fallback.message;
	const code = Object.hasOwn(record, 'code') ? record.code : undefined;
	const retryable = Object.hasOwn(record, 'retryable') && typeof record.retryable === 'boolean'
		? record.retryable : fallback.retryable;
	if (typeof code === 'string') {
		const known = KNOWN_ERROR_CODES.has(code) || CLIENT_LOCAL_ERROR_CODES.has(code) ||
			code === 'indexer_auth_expired';
		const authCode = code === 'unauthorized' || code === 'reauth_required' ||
			code === 'provider_auth_expired' || code === 'indexer_auth_expired';
		return { code: known && (!authCode || status === 401) ? code : 'request_failed', message, retryable };
	}
	// Legacy credential refresh signals apply only to a 401 without a string code.
	if (status === 401 && (legacy === 'provider_auth_expired' || legacy === 'indexer_auth_expired')) {
		return { code: legacy, message, retryable };
	}
	// Legacy body without a code: derive category and retry policy from status.
	return { code: fallback.code, message, retryable: fallback.retryable };
}
