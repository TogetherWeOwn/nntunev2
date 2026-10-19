/**
 * Shared capped JSON body reader for the web API and billing clients.
 *
 * Both `ApiClient` (`api/client.ts`) and the billing client
 * (`billing/billingClient.ts`) fetch from the same backend surface and must
 * bound how much they buffer before parsing: the per-request deadline bounds
 * time, not bytes, so an arbitrarily large body would otherwise be fully
 * buffered and parsed via an uncapped `response.json()`.
 *
 * Contract:
 * - The cap counts BYTES on the wire (UTF-8), never characters, and never
 *   trusts `Content-Length`: chunked/streamed bodies are cut by actual bytes.
 * - Reading stops early: once the cap is exceeded the stream reader is
 *   cancelled so the remainder is never consumed.
 * - Exactly `MAX_JSON_BODY_BYTES` parses normally; one byte more rejects.
 * - Over-cap rejects with {@link JsonBodyTooLargeError}, never a
 *   `SyntaxError`, so callers map it to their existing typed error shape
 *   (non-retryable, no paywall, no body bytes in the message).
 */

/** 1 MiB cap for every JSON body read through {@link readCappedJson}. */
export const MAX_JSON_BODY_BYTES = 1_048_576;

/**
 * Thrown by {@link readCappedJson} when the body exceeds
 * {@link MAX_JSON_BODY_BYTES}. Callers translate it into their typed error
 * (`ApiError` / `BillingApiError`) with `request_failed`, `retryable: false`,
 * and no paywall — never surfaced directly and never retried by default.
 */
export class JsonBodyTooLargeError extends Error {
	constructor(sizeHint = MAX_JSON_BODY_BYTES) {
		super(`response body exceeds ${sizeHint} byte limit`);
		this.name = 'JsonBodyTooLargeError';
	}
}

/** Type guard for the over-cap failure (distinguished from SyntaxError). */
export function isJsonBodyTooLarge(error: unknown): error is JsonBodyTooLargeError {
	return error instanceof JsonBodyTooLargeError;
}

/**
 * Read and parse a `Response` as JSON without buffering more than
 * {@link MAX_JSON_BODY_BYTES} bytes.
 *
 * Real responses are consumed through the body stream so an over-cap body is
 * cancelled early instead of buffered to the end. Responses without a body
 * stream (test doubles that only stub `json()`) fall back to `response.json()`
 * — those carry no bytes to cap and keep the existing decode-failure shape.
 *
 * @throws {JsonBodyTooLargeError} when the body exceeds the byte cap.
 * @throws {SyntaxError} when the capped bytes are not valid JSON.
 */
export async function readCappedJson(response: Response): Promise<unknown> {
	const stream = response.body;
	if (!stream || typeof stream.getReader !== 'function') {
		return await response.json();
	}
	const reader = stream.getReader();
	const chunks: Uint8Array[] = [];
	let totalBytes = 0;
	const decoder = new TextDecoder();
	try {
		for (;;) {
			const { done, value } = await reader.read();
			if (done) break;
			if (!value) continue;
			const bytes: Uint8Array =
				typeof value === 'string' ? new TextEncoder().encode(value) : (value as Uint8Array);
			totalBytes += bytes.byteLength;
			if (totalBytes > MAX_JSON_BODY_BYTES) {
				// Stop the upstream from producing the rest: the cap is a
				// byte bound, not a buffer-then-reject check.
				await reader.cancel().catch(() => undefined);
				throw new JsonBodyTooLargeError();
			}
			chunks.push(bytes);
		}
	} catch (error) {
		if (isJsonBodyTooLarge(error)) throw error;
		// Transport/abort failures propagate untouched so callers keep them
		// distinguishable (and retryable) from decode failures.
		throw error;
	} finally {
		reader.releaseLock();
	}
	const combined = new Uint8Array(totalBytes);
	let offset = 0;
	for (const chunk of chunks) {
		combined.set(chunk, offset);
		offset += chunk.byteLength;
	}
	return JSON.parse(decoder.decode(combined));
}
