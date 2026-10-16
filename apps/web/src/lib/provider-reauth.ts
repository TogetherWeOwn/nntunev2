/**
 * Inline provider re-auth prompt (S7).
 *
 * The API worker returns `provider_auth_expired` (HTTP 401) when the stored
 * provider credential is stale (NNTP 480/481 upstream), and the distinct
 * `indexer_auth_expired` when the indexer rejects its API key (HTTP 401
 * upstream). The web app renders an inline prompt naming the right secret
 * for each code — never a generic error page, and never the provider
 * password field for an indexer failure. No credential material lives here:
 * only the documented codes and human-readable copy.
 */

/** Documented worker/API code for stale provider credentials. */
export const PROVIDER_AUTH_EXPIRED_CODE = 'provider_auth_expired';

/** Minimal shape carrying an API error code. */
export interface CodedError {
	code: string;
}

/** Documented worker/API code for a stale indexer API key. */
export const INDEXER_AUTH_EXPIRED_CODE = 'indexer_auth_expired';

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
		return 'Your provider password has expired. Re-enter it below — your session stays signed in, and we test the new password before saving it.';
	}
	if (code === INDEXER_AUTH_EXPIRED_CODE) {
		return 'Your indexer rejected its API key. Go back to the indexer step and re-enter the API key — this is separate from your provider password.';
	}
	return null;
}
