/**
 * WebAuthn wire-format helpers shared by the signup and login pages.
 *
 * The API speaks base64url (no padding); the `navigator.credentials` API
 * wants raw bytes (`BufferSource`). Centralized here so both pages decode
 * identically.
 */

/** Decode a base64url string to raw bytes. */
export function decodeBase64Url(value: string): Uint8Array<ArrayBuffer> {
	const base64 = value.replace(/-/g, '+').replace(/_/g, '/');
	const padded = base64.padEnd(base64.length + ((4 - (base64.length % 4)) % 4), '=');
	const binary = atob(padded);
	const bytes = new Uint8Array(binary.length);
	for (let i = 0; i < binary.length; i++) bytes[i] = binary.charCodeAt(i);
	return bytes;
}
