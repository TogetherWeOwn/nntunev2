/**
 * Inline provider re-auth prompt (S7).
 *
 * The web app recognizes `provider_auth_expired` for an inline provider
 * password prompt and `indexer_auth_expired` for indexer API-key guidance.
 * Core/stream classify provider NNTP auth failures; ingest classifies
 * indexer 401s. API route wiring for those worker errors is separate. This
 * module contains only error codes and human-readable copy, never credential
 * material.
 */

import {
	INDEXER_AUTH_EXPIRED_CODE,
	PROVIDER_AUTH_EXPIRED_CODE
} from './api/client.js';

export { INDEXER_AUTH_EXPIRED_CODE, PROVIDER_AUTH_EXPIRED_CODE };

/** Minimal shape carrying an API error code. */
export interface CodedError {
	code: string;
}

/** True only for the documented provider re-auth code. */
export function isProviderAuthExpired(error: CodedError | null | undefined): boolean {
	return error?.code === PROVIDER_AUTH_EXPIRED_CODE;
}

/** True only for the documented indexer re-key code. */
export function isIndexerAuthExpired(error: CodedError | null | undefined): boolean {
	return error?.code === INDEXER_AUTH_EXPIRED_CODE;
}

/**
 * Inline re-auth copy for an API error code, or null when the failure is not
 * a stale credential. Callers render the returned string in place (keeping
 * the provider form mounted) instead of navigating to a generic error page.
 */
export function reauthPromptFor(code: string | null | undefined): string | null {
	if (code === PROVIDER_AUTH_EXPIRED_CODE) {
		return 'Your provider password has expired. Re-enter it below — your session stays signed in.';
	}
	if (code === INDEXER_AUTH_EXPIRED_CODE) {
		return 'Your indexer rejected its API key. Go back to the indexer step and re-enter the API key — this is separate from your provider password.';
	}
	return null;
}
