import { describe, expect, it } from 'vitest';

import { ApiError } from './api/client.js';
import {
	PROVIDER_AUTH_EXPIRED_CODE,
	isProviderReauthRequired,
	providerReauthPromptFor
} from './provider-reauth.js';

/**
 * Re-auth prompt contract: a `provider_auth_expired` failure renders an
 * INLINE re-auth prompt (re-enter the password in place), never a generic
 * error page.
 *
 * Test-only card: no `src/` changes here. This file FAILS (unresolved
 * import) until the builder adds `src/lib/provider-reauth.ts` with the
 * contract below. The builder MUST NOT edit this file; implement `src/`
 * to satisfy it. The Svelte wizard wires the prompt inline using these
 * helpers (no navigation, no full-page error).
 *
 * ## Builder contract (`src/lib/provider-reauth.ts`)
 *
 * ```ts
 * // Documented code, shared verbatim with `workers/api`.
 * export const PROVIDER_AUTH_EXPIRED_CODE = 'provider_auth_expired';
 *
 * // True only for the stale-credential failure (ApiError with the
 * // documented code, any 401 carrying it). Everything else is false.
 * export function isProviderReauthRequired(error: unknown): boolean;
 *
 * // Inline prompt state for the wizard. `kind` stays 'provider-reauth'
 * // so the UI renders the prompt in place. Title/body are static copy
 * // naming password re-entry; they never echo server text or secrets.
 * export interface ProviderReauthPrompt {
 *   kind: 'provider-reauth';
 *   title: string;
 *   body: string;
 * }
 * export function providerReauthPromptFor(error: unknown): ProviderReauthPrompt | null;
 * ```
 *
 * Fixtures are synthetic only; no live provider traffic.
 */

function expiredError(message = 'provider credentials expired'): ApiError {
	return new ApiError(401, 'provider_auth_expired', message);
}

describe('provider re-auth prompt', () => {
	it('pins the documented code shared with the worker', () => {
		expect(PROVIDER_AUTH_EXPIRED_CODE).toBe('provider_auth_expired');
	});

	it('requires inline re-auth for the expired-credential failure', () => {
		expect(isProviderReauthRequired(expiredError())).toBe(true);

		const prompt = providerReauthPromptFor(expiredError());
		expect(prompt).not.toBeNull();
		expect(prompt?.kind).toBe('provider-reauth');
		expect(prompt?.title.length).toBeGreaterThan(0);
		expect(prompt?.body.length).toBeGreaterThan(0);
		// The prompt names password re-entry (in-place rotation), not a
		// generic "something went wrong" page.
		expect(`${prompt?.title} ${prompt?.body}`.toLowerCase()).toContain('password');
	});

	it('stays on the generic error path for every other failure', () => {
		const others: unknown[] = [
			new ApiError(401, 'unauthorized', 'nope'),
			new ApiError(403, 'forbidden', 'nope'),
			new ApiError(404, 'not_found', 'nope'),
			new ApiError(429, 'rate_limited', 'nope'),
			new ApiError(500, 'request_failed', 'boom'),
			new Error('boom'),
			null,
			undefined
		];
		for (const error of others) {
			expect(isProviderReauthRequired(error), `reauth for ${String(error)}`).toBe(false);
			expect(providerReauthPromptFor(error), `prompt for ${String(error)}`).toBeNull();
		}
	});

	it('never echoes server text or credential material in the prompt', () => {
		const prompt = providerReauthPromptFor(
			expiredError('oops synthetic-provider-password and synthetic-indexer-key leaked upstream')
		);
		const rendered = `${prompt?.title} ${prompt?.body}`;
		expect(rendered).not.toContain('synthetic-provider-password');
		expect(rendered).not.toContain('synthetic-indexer-key');
		expect(rendered).not.toContain('oops');
	});
});
