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
	allowInsecurePlaintext: boolean;
	username: string;
	password: string;
	maxConnections: number;
}

export const MAX_CONNECTIONS_LIMIT = 50;

/** NNTP(S) ports the API worker accepts (mirrors ALLOWED_NNTP_PORTS in vault.rs). */
export const ALLOWED_NNTP_PORTS: readonly number[] = [119, 563];

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
		(a === 198 && (b === 18 || b === 19 || (b === 51 && nums[2] === 100))) ||
		(a === 192 && b === 0) ||
		(a === 203 && b === 0 && nums[2] === 113) ||
		a >= 224
	);
}

function isBlockedIpv6(host: string): boolean {
	const lower = host.toLowerCase().replace(/^\[|\]$/g, '');
	if (!lower.includes(':')) return false;
	if (lower.startsWith('::')) return true;

	const [firstText, secondText = ''] = lower.split(':');
	const firstHextet = Number.parseInt(firstText ?? '', 16);
	const secondHextet = Number.parseInt(secondText || '0', 16);
	return (
		!Number.isFinite(firstHextet) ||
		!Number.isFinite(secondHextet) ||
		(firstHextet & 0xe000) !== 0x2000 ||
		firstHextet === 0x2002 ||
		(firstHextet === 0x2001 && secondHextet < 0x0200) ||
		(firstHextet === 0x2001 && secondHextet === 0x0db8) ||
		(firstHextet === 0x3fff && (secondHextet & 0xf000) === 0)
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

	if (!Number.isInteger(form.port) || !ALLOWED_NNTP_PORTS.includes(form.port)) {
		errors.push('Port must be 119 or 563.');
	}
	if (!form.tls && !form.allowInsecurePlaintext) {
		errors.push(
			'Disabling TLS sends your password in cleartext. Tick the acknowledgement to continue without TLS.'
		);
	}
	// Rust String.len() counts UTF-8 bytes, not JavaScript UTF-16 code units.
	const encoder = new TextEncoder();
	const username = form.username.trim();
	if (username.length === 0) {
		errors.push('Enter your provider username.');
	} else if (encoder.encode(username).byteLength > 255) {
		errors.push('Provider username must be at most 255 UTF-8 bytes.');
	}
	if (form.password.length === 0) {
		errors.push('Enter your provider password.');
	} else if (encoder.encode(form.password).byteLength > 1024) {
		errors.push('Provider password must be at most 1024 UTF-8 bytes.');
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
export function validateIndexerUrl(url: string, apiKey: string): string | null {
	const value = url.trim();
	if (value.length === 0) {
		return apiKey.trim().length === 0 ? null : 'Clear the indexer key or enter an indexer URL.';
	}
	let parsed: URL;
	try {
		parsed = new URL(value);
	} catch {
		return 'That indexer URL does not look valid.';
	}
	if (parsed.protocol !== 'https:') {
		return 'Indexer URLs must use https.';
	}
	// Mirror workers/ingest check_base_url: the client sends the raw trimmed
	// string (not the normalized href), so reject any @/?/# in the
	// post-scheme remainder. This covers non-empty userinfo/query/fragment
	// as well as empty markers (trailing ?/#, bare @) that WHATWG URL
	// normalizes to empty parts but the fetch builder still rejects.
	const sep = value.indexOf('://');
	const rest = sep >= 0 ? value.slice(sep + 3) : value.slice(value.indexOf(':') + 1);
	if (rest.includes('@') || rest.includes('?') || rest.includes('#')) {
		return 'Put the API key in the key field, not the URL';
	}

	const hostname = parsed.hostname.toLowerCase().replace(/^\[|\]$/g, '').replace(/\.$/, '');
	if (
		hostname === 'localhost' ||
		hostname.endsWith('.localhost') ||
		isBlockedIpv4(hostname) ||
		isBlockedIpv6(hostname)
	) {
		return 'That indexer address is not allowed; use a public host.';
	}
	const key = apiKey.trim();
	if (key.length === 0) return 'Enter your indexer API key.';
	if (new TextEncoder().encode(key).byteLength > 1024) {
		return 'Indexer API key must be at most 1024 UTF-8 bytes.';
	}
	return null;
}
