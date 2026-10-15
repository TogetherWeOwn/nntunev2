import { afterEach, describe, expect, it, vi } from 'vitest';

import { readSignedInEmail, stashSignedInEmail } from './signed-in-email.js';

function stubStorage() {
	const map = new Map<string, string>();
	vi.stubGlobal('sessionStorage', {
		getItem: (key: string) => map.get(key) ?? null,
		setItem: (key: string, value: string) => void map.set(key, value)
	});
	return map;
}

describe('signed-in-email', () => {
	afterEach(() => {
		vi.unstubAllGlobals();
	});

	it('round-trips the signed-in identity through session storage', () => {
		stubStorage();
		expect(readSignedInEmail()).toBeNull();
		stashSignedInEmail('ada@example.com');
		expect(readSignedInEmail()).toBe('ada@example.com');
		// A later sign-in replaces the label.
		stashSignedInEmail('grace@example.com');
		expect(readSignedInEmail()).toBe('grace@example.com');
	});

	it('degrades to null without throwing when storage is unavailable', () => {
		vi.stubGlobal('sessionStorage', {
			getItem: () => {
				throw new Error('blocked');
			},
			setItem: () => {
				throw new Error('blocked');
			}
		});
		expect(() => stashSignedInEmail('ada@example.com')).not.toThrow();
		expect(readSignedInEmail()).toBeNull();
	});
});
