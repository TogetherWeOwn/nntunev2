import { describe, expect, it } from 'vitest';
import { actionForKey, SHORTCUT_ROWS } from './shortcuts.js';

/**
 * Acceptance-owned (SHUFREP-KEYS): `s` shuffles upcoming, `r` cycles repeat.
 * Builder continues this test-only branch; do not edit these assertions to get GREEN.
 *
 * Run from repo root:
 * pnpm --dir apps/web test src/lib/player/shortcuts.shufrep-keys.acceptance.test.ts
 * Browser companion: apps/web/tests/shufrep-keys.spec.ts (mocked demo browser).
 *
 * Contract: actionForKey('s') === 'shuffle-upcoming' and actionForKey('r') ===
 * 'cycle-repeat', matched case-insensitively like every other letter binding.
 * SHORTCUT_ROWS gains one row per key (labels naming shuffle / repeat) while
 * keeping all eight existing rows. The page reuses its existing guarded
 * shortcut path (typing-target + modal guards — no new guard semantics), so
 * guard behavior is pinned in the browser companion, not here.
 *
 * Baseline RED is a failing assertion on the returned action or the row list,
 * never a missing import: both names already exist on current main. SHUFREP-Uxx
 * fail pre-implementation; SHUFREP-C01 is a GREEN control pinning the existing
 * map against remaps (the sibling `0` RESTART lane stays deliberately unpinned).
 */

/** Widen the action union so the new bindings typecheck pre-implementation. */
function actionOf(key: string): unknown {
	return actionForKey(key);
}

/** Every key the dialog must keep rendering after the new rows land. */
const EXISTING_KEYS = ['k', 'n', 'p', 'j', 'l', 'm', '?', 'Escape'];

describe('shufrep-keys action bindings', () => {
	it('SHUFREP-U01 maps s to the shuffle-upcoming action', () => {
		expect(actionOf('s')).toBe('shuffle-upcoming');
	});

	it('SHUFREP-U02 maps r to the cycle-repeat action, distinct from shuffle', () => {
		expect(actionOf('r')).toBe('cycle-repeat');
		expect(actionOf('r')).not.toBe(actionOf('s'));
	});

	it('SHUFREP-U03 matches s and r case-insensitively so Shift does not void them', () => {
		expect(actionOf('S')).toBe('shuffle-upcoming');
		expect(actionOf('R')).toBe('cycle-repeat');
	});
});

describe('shufrep-keys dialog rows', () => {
	it('SHUFREP-U04 lists the s row naming shuffle, keeping every existing row', () => {
		const rows = SHORTCUT_ROWS.map((row) => row.key);
		expect(rows).toEqual(expect.arrayContaining(EXISTING_KEYS));
		const row = SHORTCUT_ROWS.find((candidate) => candidate.key === 's');
		expect(row).toBeDefined();
		expect(row?.label).toMatch(/shuffle/i);
	});

	it('SHUFREP-U05 lists the r row naming repeat, keeping every existing row', () => {
		const rows = SHORTCUT_ROWS.map((row) => row.key);
		expect(rows).toEqual(expect.arrayContaining(EXISTING_KEYS));
		const row = SHORTCUT_ROWS.find((candidate) => candidate.key === 'r');
		expect(row).toBeDefined();
		expect(row?.label).toMatch(/repeat/i);
	});
});

describe('shufrep-keys map control', () => {
	it('SHUFREP-C01 keeps the existing bindings and leaves other keys unbound', () => {
		expect(actionForKey('k')).toBe('toggle-play');
		expect(actionForKey('n')).toBe('next-track');
		expect(actionForKey('p')).toBe('previous-track');
		expect(actionForKey('j')).toBe('seek-back');
		expect(actionForKey('l')).toBe('seek-forward');
		expect(actionForKey('m')).toBe('toggle-mute');
		expect(actionForKey('?')).toBe('open-shortcuts');
		expect(actionForKey('Escape')).toBe('close-shortcuts');
		expect(actionForKey('x')).toBeNull();
		expect(actionForKey('q')).toBeNull();
		expect(actionForKey(' ')).toBeNull();
		expect(actionForKey('Enter')).toBeNull();
	});
});
