/**
 * Inline provider re-auth prompt (S7).
 *
 * The API worker returns `provider_auth_expired` (HTTP 401) when the stored
 * provider credential is stale (NNTP 481/482 or indexer 401/403 upstream).
 * The web app renders this inline prompt — the user re-enters their password
 * in place — never a generic error page. No credential material lives here:
 * only the documented code and human-readable copy.
 */

/** Documented worker/API code for stale provider credentials. */
export const PROVIDER_AUTH_EXPIRED_CODE = 'provider_auth_expired';

/** Minimal shape carrying an API error code. */
export interface CodedError {
	code: string;
}

/** True only for the documented re-auth code. */
export function isProviderAuthExpired(error: CodedError | null | undefined): boolean {
	return error?.code === PROVIDER_AUTH_EXPIRED_CODE;
}

/**
 * Inline re-auth copy for an API error code, or null when the failure is not
 * a stale credential. Callers render the returned string in place (keeping
 * the provider form mounted) instead of navigating to a generic error page.
 */
export function reauthPromptFor(code: string | null | undefined): string | null {
	if (code !== PROVIDER_AUTH_EXPIRED_CODE) return null;
	return 'Your provider password has expired. Re-enter it below — your session stays signed in, and we test the new password before saving it.';
}
