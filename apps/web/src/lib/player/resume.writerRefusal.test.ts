import { describe, expect, it } from 'vitest';
import { MAX_RESUME_ENTRIES, MAX_SERIALIZED_CHARS } from './localStateBounds.js';
import { createResumeStore, RESUME_STORAGE_KEY } from './resume.js';

/**
 * Builder-owned regression for the `clear()` writer path: reserialization can
 * expand values (`1e20` parses to a number that stringifies as
 * `100000000000000000000`), so a near-cap payload could rewrite over the
 * pre-parse bound and make every remaining resume unreadable. `clear()` must
 * refuse state-preservingly instead. Ordinary clears still persist.
 *
 * This file is Builder-owned review follow-up; the Test Author acceptance
 * bank (`resume.acceptance.test.ts`, `localStateBounds.acceptance.test.ts`)
 * is untouched.
 */

function fakeStorage(initial: Record<string, string> = {}) {
	const map = new Map(Object.entries(initial));
	return {
		getItem: (key: string) => map.get(key) ?? null,
		setItem: (key: string, value: string) => void map.set(key, value),
		removeItem: (key: string) => void map.delete(key)
	};
}

/** 256-entry payload valid under the cap only via exponent-notation values. */
function nearCapRaw(): { raw: string; firstId: string } {
	const ids: string[] = [];
	for (let i = 0; i < MAX_RESUME_ENTRIES; i += 1) {
		ids.push(`${String(i).padStart(3, '0')}${'x'.repeat(242)}`);
	}
	const raw = `{${ids.map((id) => `${JSON.stringify(id)}:1e20`).join(',')}}`;
	return { raw, firstId: ids[0] };
}

describe('resume clear() writer refusal', () => {
	it('preserves prior bytes when clearing would rewrite over the cap', () => {
		const { raw, firstId } = nearCapRaw();
		expect(raw.length).toBeLessThanOrEqual(MAX_SERIALIZED_CHARS);

		const storage = fakeStorage({ [RESUME_STORAGE_KEY]: raw });
		const resume = createResumeStore({ storage });
		expect(resume.positionFor(firstId)).toBe(1e20);

		resume.clear(firstId);

		// Refusal: stored bytes untouched, remaining resumes still readable.
		expect(storage.getItem(RESUME_STORAGE_KEY)).toBe(raw);
		expect(resume.positionFor('001' + 'x'.repeat(242))).toBe(1e20);
	});

	it('still persists ordinary clears under the cap', () => {
		const storage = fakeStorage();
		const resume = createResumeStore({ storage });

		resume.save('demo-track', 42.5);
		resume.save('second-signal', 20);
		resume.clear('demo-track');

		expect(resume.positionFor('demo-track')).toBeNull();
		expect(resume.positionFor('second-signal')).toBe(20);
		expect(storage.getItem(RESUME_STORAGE_KEY)).toBe('{"second-signal":20}');
	});
});
