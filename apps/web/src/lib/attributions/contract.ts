export type Attribution = {
	id: string;
	name: string;
	kind: 'package' | 'font' | 'icons' | 'data' | 'artwork';
	status: 'active' | 'planned';
	license: string;
	sourceUrl: string;
	licenseUrl: string;
	notice: string;
	obligations: string;
	evidence: string[];
	package?: { name: string; specifier: string; version: string };
};

export type AttributionInventory = { schemaVersion: 1; entries: Attribution[] };

function object(value: unknown): value is Record<string, unknown> {
	return typeof value === 'object' && value !== null && !Array.isArray(value);
}

function text(value: unknown): value is string {
	return typeof value === 'string' && value.trim().length > 0;
}

function httpsUrl(value: unknown): boolean {
	if (!text(value) || value !== value.trim()) return false;
	// Require an explicit `https://` authority in the stored string. A
	// standalone `new URL` parse normalizes authority-less forms such as
	// `https:example.org/...` to an HTTPS host, but a page using the
	// original as an href would resolve them relative to its own URL.
	// The negative lookahead also rejects an empty authority: WHATWG
	// special-URL parsing skips extra separators, so `https:///…` or
	// `https://\…` would otherwise promote the first path segment to
	// the hostname while the stored value still has no explicit authority.
	// WHATWG parsing additionally strips ASCII tab/LF/CR anywhere in the
	// input, so an embedded tab/newline would hide an empty authority
	// from the lookahead while still parsing to the promoted hostname.
	// Reject those characters anywhere in the stored string.
	if (/[\t\n\r]/.test(value)) return false;
	if (!/^https:\/\/(?![\/\\])/i.test(value)) return false;
	try {
		const url = new URL(value);
		return url.protocol === 'https:' && !!url.hostname && !url.username && !url.password;
	} catch {
		return false;
	}
}

function textArray(value: unknown): boolean {
	if (!Array.isArray(value) || value.length === 0) return false;
	for (let i = 0; i < value.length; i += 1) {
		if (!text(value[i])) return false;
	}
	return true;
}

/** Stable, ordered diagnostics; pure validation with no network or filesystem access. */
export function validateAttributionInventory(value: unknown): string[] {
	if (!object(value)) return ['inventory must be an object'];
	const errors: string[] = [];
	if (value.schemaVersion !== 1) errors.push('schemaVersion must be 1');
	if (!Array.isArray(value.entries) || value.entries.length === 0) {
		return [...errors, 'entries must be a non-empty array'];
	}
	const ids = new Set<string>();
	const packages = new Set<string>();
	// Indexed loop (not forEach) so sparse holes are diagnosed instead of
	// skipped: `new Array(1)` must fail, not pass with no diagnostics.
	for (let index = 0; index < value.entries.length; index += 1) {
		const entry = value.entries[index];
		const path = `entries[${index}]`;
		if (!object(entry)) {
			errors.push(`${path} must be an object`);
			continue;
		}
		for (const field of ['id', 'name', 'license', 'notice', 'obligations']) {
			if (!text(entry[field])) errors.push(`${path}.${field} is required`);
		}
		if (text(entry.id)) {
			if (ids.has(entry.id)) errors.push(`${path}.id is duplicated`);
			ids.add(entry.id);
		}
		if (typeof entry.kind !== 'string' || !['package', 'font', 'icons', 'data', 'artwork'].includes(entry.kind)) {
			errors.push(`${path}.kind is unknown`);
		}
		if (entry.status !== 'active' && entry.status !== 'planned') {
			errors.push(`${path}.status is unknown`);
		}
		for (const field of ['sourceUrl', 'licenseUrl']) {
			if (!httpsUrl(entry[field])) errors.push(`${path}.${field} must be an HTTPS URL`);
		}
		if (!textArray(entry.evidence)) {
			errors.push(`${path}.evidence must contain repository references`);
		}
		if (entry.kind === 'package') {
			if (entry.status !== 'active') errors.push(`${path} packages must be active`);
			const pkg = entry.package;
			if (!object(pkg) || !['name', 'specifier', 'version'].every((key) => text(pkg[key]))) {
				errors.push(`${path}.package requires name, specifier and version`);
			} else {
				if (packages.has(String(pkg.name))) errors.push(`${path}.package is duplicated`);
				packages.add(String(pkg.name));
			}
		} else if (entry.package !== undefined) {
			errors.push(`${path}.package is only valid for packages`);
		}
	}
	return errors;
}
