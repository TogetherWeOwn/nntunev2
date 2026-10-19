/**
 * Web client handling of the api auth-throttle 429 envelope (tests-only).
 *
 * The api worker's `throttled()` path (`POST /v1/auth/passkey/register/start`
 * denied by the per-IP auth-start budgets) migrates from the legacy body
 * `{"error":"rate_limited"}` to the standard error envelope
 * `{code:"rate_limited", message, retryable:true}` (see
 * `workers/api/tests/auth_throttle_envelope.rs` for the served-body pins).
 *
 * The client is code-switched (`parseErrorBody` branches on the envelope
 * `code`), so it needs no change for that migration — these cases are
 * controls that pass before and after the server fix and would fail only
 * if client parsing stopped classifying the new envelope as a retryable
 * rate limit, or stopped surfacing the envelope message. The exact
 * `message` wording is the server Builder's choice; a sample stands in.
 */
import { describe, expect, it } from 'vitest';

import { parseErrorBody } from './errors.js';

const SERVED_ENVELOPE = { code: 'rate_limited', message: 'too many requests', retryable: true };

describe('auth-throttle 429 envelope (web client)', () => {
	it('classifies the served envelope by its code with its own message and retryable', () => {
		expect(parseErrorBody(SERVED_ENVELOPE, 429)).toEqual({
			code: 'rate_limited',
			message: 'too many requests',
			retryable: true
		});
	});

	it('keeps the same classification as the legacy body it replaces', () => {
		// Migration safety: code + retry policy are identical for old and new
		// shapes, so no client branch changes behavior when the server flips.
		const legacy = parseErrorBody({ error: 'rate_limited' }, 429);
		const envelope = parseErrorBody(SERVED_ENVELOPE, 429);
		expect({ code: envelope.code, retryable: envelope.retryable }).toEqual({
			code: legacy.code,
			retryable: legacy.retryable
		});
		// The one user-visible gain: the envelope's message replaces the bare
		// code string the legacy body forced clients to display.
		expect(legacy.message).toBe('rate_limited');
		expect(envelope.message).toBe('too many requests');
	});

	it('switches on the envelope code, not on the status fallback message', () => {
		// A status-only parse would say "request failed with status 429"; the
		// code-switched parse must surface the server's message instead.
		const parsed = parseErrorBody(SERVED_ENVELOPE, 429);
		expect(parsed.message).not.toContain('request failed with status');
		expect(parsed.code).toBe('rate_limited');
	});
});
