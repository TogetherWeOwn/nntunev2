import { describe, expect, it } from 'vitest';
import { ICON_NAMES } from './icons.js';
import { DESIGN_STRINGS_EN, designString, type DesignStringKey } from './strings.js';

/**
 * String-key acceptance: every icon has a namespaced key with an
 * English default, so localisation later is a translation file — not a
 * hunt for hardcoded copy. Keys are stable dotted paths; values are
 * sentence-case UI words with no tracker or host references.
 */

describe('design strings: key coverage', () => {
	it('every icon name has a design.icon key', () => {
		const keys = Object.keys(DESIGN_STRINGS_EN);
		expect(keys).toHaveLength(ICON_NAMES.length);
		for (const name of ICON_NAMES) {
			expect(DESIGN_STRINGS_EN, `${name} has a key`).toHaveProperty(`design.icon.${name}`);
		}
	});

	it('keys are stable dotted paths', () => {
		for (const key of Object.keys(DESIGN_STRINGS_EN)) {
			expect(key, `${key} is namespaced`).toMatch(/^design\.icon\.[a-z0-9]+(-[a-z0-9]+)*$/);
		}
	});

	it('values are non-empty sentence-case words', () => {
		for (const [key, value] of Object.entries(DESIGN_STRINGS_EN)) {
			expect(value.trim(), `${key} is non-empty`).not.toBe('');
			expect(value, `${key} starts sentence-case`).toMatch(/^[A-Z]/);
		}
	});

	it('values carry no tracker or host references', () => {
		for (const [key, value] of Object.entries(DESIGN_STRINGS_EN)) {
			expect(value, `${key} has no internal reference`).not.toMatch(/TOG-\d|\/issues\/|\/agents\/|agent:\/\/|https?:\/\//i);
		}
	});

	it('designString resolves the table', () => {
		const key = 'design.icon.play' satisfies DesignStringKey;
		expect(designString(key)).toBe(DESIGN_STRINGS_EN[key]);
	});
});
