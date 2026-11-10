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
	try {
		const url = new URL(value);
		return url.protocol === 'https:' && !!url.hostname && !url.username && !url.password;
	} catch {
		return false;
	}
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
	value.entries.forEach((entry, index) => {
		const path = `entries[${index}]`;
		if (!object(entry)) {
			errors.push(`${path} must be an object`);
			return;
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
		if (!Array.isArray(entry.evidence) || !entry.evidence.length || !entry.evidence.every(text)) {
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
	});
	return errors;
}
