/**
 * Which account this browser is signed in as — for display only.
 *
 * Sign-in completion pages stash the account email here and the provider
 * wizard shows it before any credential is typed. That is login-CSRF
 * defense in depth: a user lured into someone else's session sees the
 * wrong identity before handing over provider credentials. This value
 * is never sent to the API and never trusted for authorization — the
 * HttpOnly session cookie is the credential; this is a label on it.
 */
const KEY = 'nntune:signed-in-email';

export function stashSignedInEmail(email: string): void {
	try {
		sessionStorage.setItem(KEY, email);
	} catch {
		// Display only: unavailable storage just loses the label.
	}
}

export function readSignedInEmail(): string | null {
	try {
		return sessionStorage.getItem(KEY);
	} catch {
		return null;
	}
}
