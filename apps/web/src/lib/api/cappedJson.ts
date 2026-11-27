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
 * - The cap counts UTF-8 bytes delivered by the stream, never characters, and never
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
const MAX_JSON_DEPTH = 128;

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
 * cancelled early instead of buffered to the end. Test doubles that omit
 * the body property entirely may still supply a pre-decoded JSON result.
 *
 * @throws {JsonBodyTooLargeError} when the body exceeds the byte cap.
 * @throws {SyntaxError} when the capped bytes are not valid JSON.
 */
export async function readCappedJson(response: Response, signal?: AbortSignal): Promise<unknown> {
	signal?.throwIfAborted();
	const stream = response.body;
	if (stream === undefined) return await response.json();
	if (!stream || typeof stream.getReader !== 'function') throw new SyntaxError('missing JSON body stream');

	const reader = stream.getReader();
	const cancel = () => { void reader.cancel().catch(() => undefined); };
	signal?.addEventListener('abort', cancel, { once: true });
	let buffer = new Uint8Array(0);
	let totalBytes = 0;
	try {
		for (;;) {
			const { done, value } = await reader.read();
			signal?.throwIfAborted();
			if (done) break;
			if (!(value instanceof Uint8Array)) {
				cancel();
				throw new SyntaxError('invalid JSON body chunk');
			}
			const nextSize = totalBytes + value.byteLength;
			if (nextSize > MAX_JSON_BODY_BYTES) {
				// An underlying source can stall its cancellation promise.
				cancel();
				throw new JsonBodyTooLargeError();
			}
			if (nextSize > buffer.length) {
				const capacity = Math.min(MAX_JSON_BODY_BYTES, Math.max(nextSize, buffer.length * 2, 4096));
				const grown = new Uint8Array(capacity);
				grown.set(buffer);
				buffer = grown;
			}
			buffer.set(value, totalBytes);
			totalBytes = nextSize;
		}
	} finally {
		signal?.removeEventListener('abort', cancel);
		reader.releaseLock();
	}

	let text: string;
	try {
		text = new TextDecoder('utf-8', { fatal: true }).decode(buffer.subarray(0, totalBytes));
	} catch {
		throw new SyntaxError('invalid JSON body encoding');
	}
	// Bound nesting for callers that traverse decoded JSON recursively.
	// JSON.parse still validates the grammar after the depth check.
	let depth = 0;
	let inString = false;
	for (let index = 0; index < text.length; index++) {
		const char = text[index];
		if (inString && char === '\\') {
			index++;
		} else if (char === '"') {
			inString = !inString;
		} else if (!inString) {
			if (char === '{' || char === '[') {
				if (++depth > MAX_JSON_DEPTH) throw new SyntaxError('JSON body nesting limit exceeded');
			} else if (char === '}' || char === ']') {
				depth--;
			}
		}
	}
	return JSON.parse(text);
}
