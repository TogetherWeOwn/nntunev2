/** Production-only bindings extend, but do not change, the demo shortcut contract. */
import { actionForKey, SHORTCUT_ROWS, type ShortcutAction, type ShortcutRow } from './shortcuts.js';

export function actionForPlayerKey(key: string): ShortcutAction | 'toggle-favorite' | null {
	return key.toLowerCase() === 'f' ? 'toggle-favorite' : actionForKey(key);
}

export const PLAYER_SHORTCUT_ROWS: readonly ShortcutRow[] = [
	...SHORTCUT_ROWS,
	{ key: 'f', label: 'Save / remove current track from Favorites' }
];
