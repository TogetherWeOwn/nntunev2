import { describe, expect, it } from 'vitest';
import { actionForPlayerKey, PLAYER_SHORTCUT_ROWS } from './appShortcuts.js';
import { actionForKey, SHORTCUT_ROWS, type ShortcutAction } from './shortcuts.js';

const LEGACY_BINDINGS: ReadonlyArray<readonly [string, ShortcutAction]> = [
	['k', 'toggle-play'],
	['n', 'next-track'],
	['p', 'previous-track'],
	['j', 'seek-back'],
	['l', 'seek-forward'],
	['0', 'restart-track'],
	['m', 'toggle-mute'],
	['s', 'shuffle-upcoming'],
	['r', 'cycle-repeat'],
	['?', 'open-shortcuts'],
	['Escape', 'close-shortcuts']
];

describe('actionForPlayerKey', () => {
	it('maps f and F to toggle-favorite', () => {
		expect(actionForPlayerKey('f')).toBe('toggle-favorite');
		expect(actionForPlayerKey('F')).toBe('toggle-favorite');
	});

	it.each(LEGACY_BINDINGS)('delegates %s unchanged to %s', (key, action) => {
		expect(actionForPlayerKey(key)).toBe(action);
		expect(actionForPlayerKey(key)).toBe(actionForKey(key));
	});

	it('delegates uppercase legacy letters the same as the demo map', () => {
		for (const key of ['K', 'N', 'P', 'J', 'L', 'M', 'S', 'R']) {
			expect(actionForPlayerKey(key)).toBe(actionForKey(key));
			expect(actionForPlayerKey(key)).not.toBeNull();
		}
	});

	it('returns null for unmapped and multi-character keys', () => {
		for (const key of ['x', 'z', '1', '', ' ', 'Enter', 'ArrowLeft', 'ff', 'F1', 'escape']) {
			expect(actionForPlayerKey(key)).toBeNull();
		}
	});
});

describe('PLAYER_SHORTCUT_ROWS', () => {
	it('contains each legacy row unchanged', () => {
		for (const row of SHORTCUT_ROWS) {
			expect(PLAYER_SHORTCUT_ROWS).toContainEqual(row);
		}
		expect(PLAYER_SHORTCUT_ROWS.length).toBe(SHORTCUT_ROWS.length + 1);
	});

	it('has exactly one f row with a meaningful Favorites label', () => {
		const fRows = PLAYER_SHORTCUT_ROWS.filter((row) => row.key.toLowerCase() === 'f');
		expect(fRows).toHaveLength(1);
		expect(fRows[0].key).toBe('f');
		expect(fRows[0].label.trim().length).toBeGreaterThan(5);
		expect(fRows[0].label).toMatch(/favorites/i);
	});
});

describe('demo shortcut contract stays unchanged', () => {
	it('actionForKey does not bind f or F', () => {
		expect(actionForKey('f')).toBeNull();
		expect(actionForKey('F')).toBeNull();
	});

	it('SHORTCUT_ROWS has no f row and keeps the legacy keys', () => {
		expect(SHORTCUT_ROWS.some((row) => row.key.toLowerCase() === 'f')).toBe(false);
		expect(SHORTCUT_ROWS.map((row) => row.key)).toEqual([
			'k', 'n', 'p', 'j', 'l', '0', 'm', 's', 'r', '?', 'Escape'
		]);
	});
});
