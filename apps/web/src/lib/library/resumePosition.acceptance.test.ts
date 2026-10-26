import { afterEach, describe, expect, it, vi } from 'vitest';
import { createResumeStore } from '../player/resume.js';
import { invalidateLibraryCache } from './cache.js';

/**
 * Protected pure companion to tests/library-resume-position.spec.ts.
 * Exercises SHIPPED APIs only. No missing new-module import can masquerade as RED.
 * New integration RED: the real auth-boundary cleanup leaves scoped library
 * resume bytes behind. Existing-store controls prove the reused shape/bounds.
 *
 * Breaks caught: failing to clear the outgoing resume key before revocation;
 * clearing another tab/demo; accepting oversized input; prototype-key loss;
 * quota refusal damaging previous bytes; leaking identities in scoped payloads.
 * Builder must not edit this file. No source-text or helper-path pins.
 */
const CONTEXT_KEY = 'nntune.library.auth-context.v1';
const CONTEXT = 'synthetic-tab-nonce';
const KEY = `nntune.library.resume.v1.${CONTEXT}`;
const OTHER_KEY = 'nntune.library.resume.v1.different-tab-nonce';
const DEMO_KEY = 'nntune:resume:v1';

function storage(initial: Array<[string, string]> = []) {
	const values = new Map(initial);
	const calls: Array<{ op: string; key: string; value?: string }> = [];
	return {
		getItem(key: string) { calls.push({ op: 'get', key }); return values.get(key) ?? null; },
		setItem(key: string, value: string) { calls.push({ op: 'set', key, value }); values.set(key, value); },
		removeItem(key: string) { calls.push({ op: 'remove', key }); values.delete(key); },
		raw: (key: string) => values.get(key) ?? null,
		calls
	};
}
function note(id: string, actual: unknown) {
	return JSON.stringify({ case: id, expected: 'outgoing scoped resume cleared before auth context revocation', actual,
		status: 'RED', error_code: 'AUTH_RESUME_BYTES_RETAINED',
		next_action: 'Integrate resume cleanup into existing auth-boundary handling; do not edit this bank.' });
}
afterEach(() => { vi.unstubAllGlobals(); vi.restoreAllMocks(); });

describe('authenticated-tab resume boundaries', () => {
	it('LIBRESUME-24 auth cleanup removes outgoing bytes before revocation without decoding them', () => {
		const session = storage([[CONTEXT_KEY, CONTEXT], [KEY, '{corrupt private-looking synthetic data'], [OTHER_KEY, '{"other":3}'], [DEMO_KEY, '{"demo":4}']]);
		const local = storage([['unrelated', 'preserve']]);
		vi.stubGlobal('window', { sessionStorage: session, localStorage: local });
		invalidateLibraryCache();
		expect(session.raw(KEY), note('LIBRESUME-24', session.raw(KEY))).toBeNull();
		expect(session.raw(CONTEXT_KEY)).toBeNull();
		const cleared = session.calls.findIndex((call) => call.key === KEY && call.op === 'remove');
		const revoked = session.calls.findIndex((call) => call.key === CONTEXT_KEY && call.op === 'remove');
		expect(cleared).toBeGreaterThanOrEqual(0); expect(revoked).toBeGreaterThan(cleared);
		expect(session.raw(OTHER_KEY)).toBe('{"other":3}'); expect(session.raw(DEMO_KEY)).toBe('{"demo":4}');
		expect(local.raw('unrelated')).toBe('preserve');
	});

	it('LIBRESUME-25 throwing cleanup is contained and does not prevent context revocation', () => {
		const session = storage([[CONTEXT_KEY, CONTEXT], [KEY, '{"track":9}']]);
		const remove = session.removeItem;
		const attempted: string[] = [];
		session.removeItem = (key) => { attempted.push(key); if (key === KEY) throw new Error('synthetic storage refusal'); remove(key); };
		vi.stubGlobal('window', { sessionStorage: session, localStorage: storage() });
		expect(() => invalidateLibraryCache()).not.toThrow();
		expect(session.raw(CONTEXT_KEY)).toBeNull();
		expect(attempted, note('LIBRESUME-25', attempted)).toContain(KEY);
	});

	it('LIBRESUME-26 reused store keeps exact seconds and only opaque IDs in the selected session key', () => {
		const backing = storage([[DEMO_KEY, '{"track":99}'], [OTHER_KEY, '{"track":88}']]);
		const resume = createResumeStore({ storage: backing, storageKey: KEY });
		expect(resume.positionFor('track')).toBeNull(); resume.save('track', 42.5);
		expect(backing.raw(KEY)).toBe('{"track":42.5}');
		expect(createResumeStore({ storage: backing, storageKey: KEY }).positionFor('track')).toBe(42.5);
		expect(backing.raw(DEMO_KEY)).toBe('{"track":99}'); expect(backing.raw(OTHER_KEY)).toBe('{"track":88}');
		expect(backing.calls.every((call) => call.key === KEY)).toBe(true);
	});

	it('LIBRESUME-27 prototype-like opaque IDs round-trip and clear independently', () => {
		const backing = storage(); const resume = createResumeStore({ storage: backing, storageKey: KEY });
		for (const [id, seconds] of [['__proto__', 12.5], ['constructor', 23], ['toString', 34]] as const) resume.save(id, seconds);
		expect(Object.entries(JSON.parse(backing.raw(KEY)!))).toEqual([['__proto__', 12.5], ['constructor', 23], ['toString', 34]]);
		resume.clear('__proto__'); resume.clear('__proto__');
		expect(resume.positionFor('__proto__')).toBeNull(); expect(resume.positionFor('constructor')).toBe(23);
		expect(Object.hasOwn(Object.prototype, 'polluted')).toBe(false);
	});

	it('LIBRESUME-28 raw input at the existing byte cap decodes; over-cap refuses before parsing', () => {
		const json = '{"track":7}'; const atCap = json + ' '.repeat(65_536 - json.length);
		const backing = storage([[KEY, atCap]]);
		expect(createResumeStore({ storage: backing, storageKey: KEY }).positionFor('track')).toBe(7);
		backing.setItem(KEY, atCap + ' '); const parse = vi.spyOn(JSON, 'parse');
		expect(createResumeStore({ storage: backing, storageKey: KEY }).positionFor('track')).toBeNull();
		expect(parse).not.toHaveBeenCalled();
	});

	it('LIBRESUME-29 entry-cap refusal is state-preserving, but overwriting at capacity works', () => {
		const data = Object.fromEntries(Array.from({ length: 256 }, (_, i) => [`track-${i}`, i]));
		const raw = JSON.stringify(data); const backing = storage([[KEY, raw]]);
		const resume = createResumeStore({ storage: backing, storageKey: KEY });
		resume.save('overflow', 9); expect(backing.raw(KEY)).toBe(raw);
		resume.save('track-0', 11); expect(resume.positionFor('track-0')).toBe(11);
		backing.setItem(KEY, JSON.stringify({ ...data, overflow: 9 }));
		expect(createResumeStore({ storage: backing, storageKey: KEY }).positionFor('track-0')).toBeNull();
	});

	it('LIBRESUME-30 opaque-ID cap refuses an overlong ID without changing previous bytes', () => {
		const backing = storage(); const resume = createResumeStore({ storage: backing, storageKey: KEY });
		resume.save('x'.repeat(256), 6); const before = backing.raw(KEY);
		expect(resume.positionFor('x'.repeat(256))).toBe(6);
		resume.save('x'.repeat(257), 7); expect(backing.raw(KEY)).toBe(before);
		expect(resume.positionFor('x'.repeat(257))).toBeNull();
	});

	it.each([NaN, Infinity, -Infinity, -1])('LIBRESUME-31 invalid seconds %s cannot damage a saved value', (seconds) => {
		const backing = storage([[KEY, '{"track":8}']]);
		createResumeStore({ storage: backing, storageKey: KEY }).save('track', seconds);
		expect(backing.raw(KEY)).toBe('{"track":8}');
		expect(backing.calls.filter((call) => call.op !== 'get')).toEqual([]);
	});

	it('LIBRESUME-32 denied writes never spill into demo/local/other-context storage', () => {
		const backing = storage([[KEY, '{"track":8}']]);
		const attempted: string[] = [];
		backing.setItem = (key) => { attempted.push(key); throw new Error('synthetic quota'); };
		const resume = createResumeStore({ storage: backing, storageKey: KEY });
		expect(() => resume.save('track', 9)).not.toThrow(); expect(backing.raw(KEY)).toBe('{"track":8}');
		expect(attempted).toEqual([KEY]);
		expect(backing.calls.every((call) => call.key === KEY)).toBe(true);
	});
});
