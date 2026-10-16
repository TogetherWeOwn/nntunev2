/**
 * Shared parsing for the workers' structured error envelope
 * (`{code, message, retryable}`, registered in `docs/api-errors.md`).
 *
 * Unknown codes fall back to the documented generic (`request_failed`
 * client-side) while keeping the server `message` for display. Legacy
 * `{"error": "..."}` bodies are still accepted during migration: the code
 * is then derived from the HTTP status.
 */

/** Every code in the `docs/api-errors.md` registry. */
export const KNOWN_ERROR_CODES = new Set([
	'unauthorized',
	'plan_limit_exceeded',
	'forbidden',
	'not_found',
	'invalid_request',
	'payload_too_large',
	'range_not_satisfiable',
	'validation_failed',
	'rate_limited',
	'internal_error',
	'corrupt_content',
	'upstream_unavailable',
	'service_unavailable'
]);

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
	if (!body || typeof body !== 'object') return fallback;
	const record = body as { code?: unknown; message?: unknown; retryable?: unknown; error?: unknown };
	const message =
		typeof record.message === 'string' && record.message.length > 0
			? record.message
			: typeof record.error === 'string' && record.error.length > 0
				? record.error
				: fallback.message;
	for (const candidate of [record.code, record.error]) {
		if (candidate === 'provider_auth_expired' || candidate === 'indexer_auth_expired') {
			return {
				code: candidate,
				message,
				retryable: typeof record.retryable === 'boolean' ? record.retryable : fallback.retryable
			};
		}
	}
	if (typeof record.code === 'string' && KNOWN_ERROR_CODES.has(record.code)) {
		return {
			code: record.code,
			message,
			retryable: typeof record.retryable === 'boolean' ? record.retryable : fallback.retryable
		};
	}
	if (typeof record.code === 'string') {
		// Unknown envelope code: do not infer a different category from its status.
		return {
			code: 'request_failed',
			message,
			retryable: typeof record.retryable === 'boolean' ? record.retryable : fallback.retryable
		};
	}
	// Legacy body without a code: derive category and retry policy from status.
	return { code: fallback.code, message, retryable: fallback.retryable };
}

/** Read and parse an error `Response` body exactly once. */
export async function parseErrorResponse(response: Response): Promise<ParsedApiError> {
	try {
		return parseErrorBody(await response.json(), response.status);
	} catch {
		return parseErrorBody(null, response.status);
	}
}
