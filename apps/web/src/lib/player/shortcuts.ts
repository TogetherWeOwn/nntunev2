/**
 * Global transport keyboard shortcuts for the demo player.
 *
 * Pure key/guard helpers only — no DOM globals, no listeners — so unit
 * tests can pin the map with plain fakes. The page owns the listener and
 * the transport calls. Guards are structural (tag names, rect counts) on
 * purpose: real DOM nodes satisfy them and so do test doubles.
 */

export type ShortcutAction =
	| 'toggle-play'
	| 'next-track'
	| 'previous-track'
	| 'seek-back'
	| 'seek-forward'
	| 'restart-track'
	| 'toggle-mute'
	| 'shuffle-upcoming'
	| 'cycle-repeat'
	| 'open-shortcuts'
	| 'close-shortcuts';

/** Single-character bindings matched against `KeyboardEvent.key`. */
const KEY_BINDINGS: Readonly<Record<string, ShortcutAction>> = {
	k: 'toggle-play',
	n: 'next-track',
	p: 'previous-track',
	j: 'seek-back',
	l: 'seek-forward',
	'0': 'restart-track',
	m: 'toggle-mute',
	s: 'shuffle-upcoming',
	r: 'cycle-repeat',
	'?': 'open-shortcuts'
};

/** Seconds each seek binding moves the demo audio. */
export const SEEK_SECONDS = 10;

/** Resolve a key to its shortcut action, or null when unbound. */
export function actionForKey(key: string): ShortcutAction | null {
	if (key === 'Escape') return 'close-shortcuts';
	if (key.length !== 1) return null;
	return KEY_BINDINGS[key.toLowerCase()] ?? null;
}

/**
 * True while the key event targets a typing surface (input, textarea,
 * select/option or contenteditable region). `isContentEditable` is inherited,
 * so targets nested inside an editable region are covered too. Native
 * selectors keep type-ahead/Space/arrow ownership; ordinary buttons, audio
 * and body still permit shortcuts.
 */
export function isTypingTarget(target: unknown): boolean {
	if (typeof target !== 'object' || target === null) return false;
	const candidate = target as { isContentEditable?: unknown; tagName?: unknown };
	if (candidate.isContentEditable === true) return true;
	return (
		candidate.tagName === 'INPUT' ||
		candidate.tagName === 'TEXTAREA' ||
		candidate.tagName === 'SELECT' ||
		candidate.tagName === 'OPTION'
	);
}

/**
 * Minimal surface the modal guard needs; the real `Document` satisfies
 * it (`querySelector` result widens to `unknown`, `NodeListOf` is an
 * `ArrayLike`).
 */
export interface ModalScope {
	querySelector(selectors: string): unknown;
	querySelectorAll(selectors: string): ArrayLike<unknown>;
}

function isRendered(candidate: unknown): boolean {
	if (typeof candidate !== 'object' || candidate === null) return false;
	const element = candidate as { getClientRects?: () => ArrayLike<unknown> };
	return typeof element.getClientRects === 'function' && element.getClientRects().length > 0;
}

/** True while a modal dialog (native or `role="dialog"`) captures keys. */
export function isModalOpen(scope: ModalScope): boolean {
	if (scope.querySelector('dialog[open]')) return true;
	const dialogs = scope.querySelectorAll('[role="dialog"]');
	for (let index = 0; index < dialogs.length; index += 1) {
		if (isRendered(dialogs[index])) return true;
	}
	return false;
}

export type ShortcutRow = { key: string; label: string };

/** Single source for the shortcuts dialog listing. */
export const SHORTCUT_ROWS: readonly ShortcutRow[] = [
	{ key: 'k', label: 'Play / pause' },
	{ key: 'n', label: 'Next track' },
	{ key: 'p', label: 'Previous track' },
	{ key: 'j', label: 'Seek back 10 seconds' },
	{ key: 'l', label: 'Seek forward 10 seconds' },
	{ key: '0', label: 'Restart track' },
	{ key: 'm', label: 'Mute / unmute' },
	{ key: 's', label: 'Shuffle upcoming tracks' },
	{ key: 'r', label: 'Cycle repeat: Off / One / Queue' },
	{ key: '?', label: 'Open this shortcuts dialog' },
	{ key: 'Escape', label: 'Close this dialog' }
];
