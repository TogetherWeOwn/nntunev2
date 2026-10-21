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

/**
 * Explicit `:port` digits from a backslash-free raw authority, or null when
 * no explicit port is present. Bracket-aware like the ingest fetch builder
 * (`split_host_port`): a leading `[` scans only after the matching `]`,
 * otherwise the first `:` starts the port. Returns `''` for empty digits
 * (`host:/path`). The caller must have banned `@`/`?`/`#` already.
 */
function rawExplicitPort(authority: string): string | null {
	if (authority.startsWith('[')) {
		const close = authority.indexOf(']');
		if (close === -1) return null;
		const after = authority.slice(close + 1);
		if (!after.startsWith(':')) return null;
		return after.slice(1);
	}
	const colon = authority.indexOf(':');
	if (colon === -1) return null;
	return authority.slice(colon + 1);
}

export type ProviderFieldErrors = Partial<Record<keyof ProviderForm, string>>;
export type IndexerFieldErrors = Partial<Record<'indexerUrl' | 'indexerApiKey', string>>;

/** Returns a list of human-readable problems; empty means valid. */
export function validateProviderConfig(form: ProviderForm): string[] {
	return Object.values(validateProviderFields(form));
}

/** Field keys are control identities, never inferred from translated error text. */
export function validateProviderFields(form: ProviderForm): ProviderFieldErrors {
	const errors: ProviderFieldErrors = {};
	const host = form.host.trim();

	if (host.length === 0) {
		errors.host = 'Enter your provider hostname.';
	} else if (host.includes('://') || host.includes('/') || /\s/.test(host)) {
		errors.host = 'Enter a hostname only, without scheme or path.';
	} else if (isBlockedIpv4(host) || isBlockedIpv6(host)) {
		errors.host = 'That IP address is not allowed; use your provider hostname.';
	} else if (/^[0-9.]+$/.test(host) || host.includes(':')) {
		errors.host = 'Use your provider hostname, not a literal IP address.';
	} else if (!HOSTNAME_PATTERN.test(host)) {
		errors.host = 'That hostname does not look valid.';
	}

	if (!Number.isInteger(form.port) || !ALLOWED_NNTP_PORTS.includes(form.port)) {
		errors.port = 'Port must be 119 or 563.';
	}
	if (!form.tls && !form.allowInsecurePlaintext) {
		errors.allowInsecurePlaintext =
			'Disabling TLS sends your password in cleartext. Tick the acknowledgement to continue without TLS.';
	}
	// Rust String.len() counts UTF-8 bytes, not JavaScript UTF-16 code units.
	const encoder = new TextEncoder();
	const username = form.username.trim();
	if (username.length === 0) {
		errors.username = 'Enter your provider username.';
	} else if (encoder.encode(username).byteLength > 255) {
		errors.username = 'Provider username must be at most 255 UTF-8 bytes.';
	}
	if (form.password.length === 0) {
		errors.password = 'Enter your provider password.';
	} else if (encoder.encode(form.password).byteLength > 1024) {
		errors.password = 'Provider password must be at most 1024 UTF-8 bytes.';
	}
	if (
		!Number.isInteger(form.maxConnections) ||
		form.maxConnections < 1 ||
		form.maxConnections > MAX_CONNECTIONS_LIMIT
	) {
		errors.maxConnections = `Connections must be between 1 and ${MAX_CONNECTIONS_LIMIT}.`;
	}
	return errors;
}

/**
 * Validate the optional indexer URL. Returns an error string or null when
 * valid. Empty means "no indexer", which is a supported setup.
 */
export function validateIndexerUrl(url: string, apiKey: string): string | null {
	return Object.values(validateIndexerFields(url, apiKey))[0] ?? null;
}

export function validateIndexerFields(url: string, apiKey: string): IndexerFieldErrors {
	const value = url.trim();
	if (value.length === 0) {
		return apiKey.trim().length === 0 ? {} : { indexerUrl: 'Clear the indexer key or enter an indexer URL.' };
	}
	// Mirror workers/ingest check_base_url on the raw trimmed value: the
	// client sends the raw string (not the normalized href), so WHATWG
	// normalization must not hide a form the fetch builder rejects. Require
	// the literal `https://` prefix, a non-empty non-slash remainder, no
	// `@`/`?`/`#`, and no whitespace (including U+0085 NEL, which JS \s
	// misses but Rust is_whitespace rejects) — before parsing.
	if (!value.startsWith('https://')) {
		try {
			const probe = new URL(value);
			if (probe.protocol !== 'https:') {
				return { indexerUrl: 'Indexer URLs must use https.' };
			}
		} catch {
			return { indexerUrl: 'That indexer URL does not look valid.' };
		}
		// Parses as https but without the literal prefix (uppercase scheme,
		// missing slash, backslash slashes, …) — downstream requires the
		// literal bytes, so reject here instead of reporting success.
		return { indexerUrl: 'Indexer URLs must use https.' };
	}
	const rest = value.slice('https://'.length);
	if (rest.length === 0 || rest.startsWith('/')) {
		return { indexerUrl: 'That indexer URL does not look valid.' };
	}
	if (rest.includes('@') || rest.includes('?') || rest.includes('#')) {
		return { indexerUrl: 'Put the API key in the key field, not the URL' };
	}
	if (/[\s\u0085]/.test(rest)) {
		return { indexerUrl: 'That indexer URL does not look valid.' };
	}
	let parsed: URL;
	try {
		parsed = new URL(value);
	} catch {
		return { indexerUrl: 'That indexer URL does not look valid.' };
	}
	if (parsed.protocol !== 'https:') {
		return { indexerUrl: 'Indexer URLs must use https.' };
	}

	const hostname = parsed.hostname.toLowerCase().replace(/^\[|\]$/g, '').replace(/\.$/, '');
	if (
		hostname === 'localhost' ||
		hostname.endsWith('.localhost') ||
		isBlockedIpv4(hostname) ||
		isBlockedIpv6(hostname)
	) {
		return { indexerUrl: 'That indexer address is not allowed; use a public host.' };
	}
	// The ingest fetch model is 443-only: an explicit port other than 443
	// must fail here with a port-specific message instead of passing client
	// validation and failing later at the server / ingest boundary. Read the
	// port from the raw authority, not just the parsed URL: WHATWG
	// normalization hides forms the fetch builder rejects — empty port
	// digits (`host:/path`) normalize to an empty `port`, and a backslash
	// reads as a path separator while Rust splits the authority only on
	// `/`/`?`/`#` — and the client sends the raw trimmed string. Empty or
	// non-numeric port digits and a backslash in the authority are malformed
	// URLs (generic message); all-digit ports other than 443 get the port
	// message. Kept after the blocked-host check so pinned diagnostics
	// (e.g. `localhost:8080`) keep their address message.
	const slash = rest.indexOf('/');
	const rawAuthority = slash === -1 ? rest : rest.slice(0, slash);
	if (rawAuthority.includes('\\')) {
		return { indexerUrl: 'That indexer URL does not look valid.' };
	}
	const explicitPort = rawExplicitPort(rawAuthority);
	if (explicitPort !== null) {
		if (explicitPort === '' || !/^\d+$/.test(explicitPort)) {
			return { indexerUrl: 'That indexer URL does not look valid.' };
		}
		// Numeric compare, mirroring the fetch builder's parse_port (digits
		// parsed as u16 before comparing against 443): leading-zero
		// spellings such as `:0443` are port 443 downstream too.
		if (Number(explicitPort) !== 443) {
			return { indexerUrl: 'Indexer URLs must use port 443.' };
		}
	} else if (parsed.port !== '') {
		// Backstop: unreachable while the raw scan agrees with WHATWG, kept
		// so a future divergence still fails closed on non-443 ports.
		return { indexerUrl: 'Indexer URLs must use port 443.' };
	}
	const key = apiKey.trim();
	if (key.length === 0) return { indexerApiKey: 'Enter your indexer API key.' };
	if (new TextEncoder().encode(key).byteLength > 1024) {
		return { indexerApiKey: 'Indexer API key must be at most 1024 UTF-8 bytes.' };
	}
	return {};
}
