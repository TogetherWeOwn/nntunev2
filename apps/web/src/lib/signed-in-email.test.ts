import { afterEach, describe, expect, it, vi } from 'vitest';

import { clearSignedInEmail, readSignedInEmail, stashSignedInEmail } from './signed-in-email.js';

function stubStorage() {
	const map = new Map<string, string>();
	vi.stubGlobal('sessionStorage', {
		getItem: (key: string) => map.get(key) ?? null,
		setItem: (key: string, value: string) => void map.set(key, value),
		removeItem: (key: string) => void map.delete(key)
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

	it('clears the label on sign-out', () => {
		stubStorage();
		stashSignedInEmail('ada@example.com');
		expect(readSignedInEmail()).toBe('ada@example.com');
		clearSignedInEmail();
		expect(readSignedInEmail()).toBeNull();
	});

	it('degrades to null without throwing when storage is unavailable', () => {
		vi.stubGlobal('sessionStorage', {
			getItem: () => {
				throw new Error('blocked');
			},
			setItem: () => {
				throw new Error('blocked');
			},
			removeItem: () => {
				throw new Error('blocked');
			}
		});
		expect(() => stashSignedInEmail('ada@example.com')).not.toThrow();
		expect(readSignedInEmail()).toBeNull();
		expect(() => clearSignedInEmail()).not.toThrow();
	});
});
