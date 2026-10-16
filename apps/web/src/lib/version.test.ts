import { describe, expect, it } from 'vitest';

import pkg from '../../package.json';
import { APP_VERSION } from './version.js';

describe('APP_VERSION', () => {
	it('matches the package version (package.json is the single source of truth)', () => {
		expect(APP_VERSION).toBe(pkg.version);
	});

	it('is a public version string with no internal identifiers', () => {
		expect(typeof APP_VERSION).toBe('string');
		expect(APP_VERSION.length).toBeGreaterThan(0);
		// No whitespace, commit SHAs, hostnames, or paths leak into the public contract.
		expect(APP_VERSION).not.toMatch(/\s/);
		expect(APP_VERSION).not.toMatch(/localhost|internal|\.local|[/\\]/);
		expect(APP_VERSION).not.toMatch(/\b[0-9a-f]{20,}\b/);
	});
});
