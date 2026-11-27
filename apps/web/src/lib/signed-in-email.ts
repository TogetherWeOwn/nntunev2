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
const revoked = new WeakSet<Storage>();

function validLabel(value: string | null): value is string {
	return typeof value === 'string' && value.length <= 254 &&
		!/\p{Bidi_Control}|\p{Cc}|\s/u.test(value) && /^[^@]+@[^@]*\.[^@]*$/.test(value);
}

export function stashSignedInEmail(email: string): void {
	try {
		const storage = sessionStorage;
		if (!validLabel(email)) return clearSignedInEmail();
		storage.setItem(KEY, email);
		revoked.delete(storage);
	} catch {
		clearSignedInEmail();
	}
}

export function readSignedInEmail(): string | null {
	try {
		const storage = sessionStorage;
		if (revoked.has(storage)) return null;
		const value = storage.getItem(KEY);
		return validLabel(value) ? value : null;
	} catch {
		return null;
	}
}

export function clearSignedInEmail(): void {
	try {
		const storage = sessionStorage;
		revoked.add(storage);
		storage.removeItem(KEY);
	} catch {
		// Display only: a missing store just leaves nothing to clear.
	}
}
