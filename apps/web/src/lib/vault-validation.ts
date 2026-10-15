/**
 * Client-side validation for the bring-your-own-provider (BYOP) wizard.
 *
 * UX-only: every rule here is re-enforced by `workers/api` (vault validation
 * + SSRF guard + envelope encryption). The client copy exists so typos fail
 * fast without a round trip. Never treat a passing client check as proof the
 * server will accept the config.
 */

export interface ProviderForm {
	host: string;
	port: number;
	tls: boolean;
	username: string;
	password: string;
	maxConnections: number;
}

export const MAX_CONNECTIONS_LIMIT = 100;

const HOSTNAME_LABEL = '[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?';
const HOSTNAME_PATTERN = new RegExp(`^${HOSTNAME_LABEL}(?:\\.${HOSTNAME_LABEL})+$`, 'i');

/** IPv4 literals the server rejects (private, loopback, link-local, …). */
function isBlockedIpv4(host: string): boolean {
	const octets = host.split('.');
	if (octets.length !== 4 || !octets.every((o) => /^\d{1,3}$/.test(o))) return false;
	const nums = octets.map(Number);
	if (nums.some((n) => n > 255)) return false;
	const [a, b] = nums;
	return (
		a === 10 ||
		a === 127 ||
		a === 0 ||
		(a === 172 && b >= 16 && b <= 31) ||
		(a === 192 && b === 168) ||
		(a === 169 && b === 254) ||
		(a === 100 && b >= 64 && b <= 127) ||
		(a === 198 && (b === 18 || b === 19)) ||
		(a === 192 && b === 0) ||
		a >= 240
	);
}

function isBlockedIpv6(host: string): boolean {
	const lower = host.toLowerCase();
	return (
		lower === '::1' ||
		lower.startsWith('fe80:') ||
		lower.startsWith('fc') ||
		lower.startsWith('fd') ||
		lower.includes('::ffff:')
	);
}

/** Returns a list of human-readable problems; empty means valid. */
export function validateProviderConfig(form: ProviderForm): string[] {
	const errors: string[] = [];
	const host = form.host.trim();

	if (host.length === 0) {
		errors.push('Enter your provider hostname.');
	} else if (host.includes('://') || host.includes('/') || /\s/.test(host)) {
		errors.push('Enter a hostname only, without scheme or path.');
	} else if (isBlockedIpv4(host) || isBlockedIpv6(host)) {
		errors.push('That IP address is not allowed; use your provider hostname.');
	} else if (/^[0-9.]+$/.test(host) || host.includes(':')) {
		errors.push('Use your provider hostname, not a literal IP address.');
	} else if (!HOSTNAME_PATTERN.test(host)) {
		errors.push('That hostname does not look valid.');
	}

	if (!Number.isInteger(form.port) || form.port < 1 || form.port > 65535) {
		errors.push('Port must be between 1 and 65535.');
	}
	if (form.username.trim().length === 0) {
		errors.push('Enter your provider username.');
	}
	if (form.password.length === 0) {
		errors.push('Enter your provider password.');
	}
	if (
		!Number.isInteger(form.maxConnections) ||
		form.maxConnections < 1 ||
		form.maxConnections > MAX_CONNECTIONS_LIMIT
	) {
		errors.push(`Connections must be between 1 and ${MAX_CONNECTIONS_LIMIT}.`);
	}
	return errors;
}

/**
 * Validate the optional indexer URL. Returns an error string or null when
 * valid. Empty means "no indexer", which is a supported setup.
 */
export function validateIndexerUrl(url: string): string | null {
	const value = url.trim();
	if (value.length === 0) return null;
	let parsed: URL;
	try {
		parsed = new URL(value);
	} catch {
		return 'That indexer URL does not look valid.';
	}
	// http is a dev-only exception for the literal name `localhost`; loopback
	// literals (127.0.0.1, ::1) stay rejected like the server rejects them.
	const isLocalhost = parsed.hostname.toLowerCase() === 'localhost';
	if (parsed.protocol === 'https:') return null;
	if (parsed.protocol === 'http:' && isLocalhost) return null;
	return 'Indexer URLs must use https (http is allowed for localhost only).';
}
