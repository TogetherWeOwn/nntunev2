import { describe, expect, it } from 'vitest';
import {
	actionForKey,
	isModalOpen,
	isTypingTarget,
	SEEK_SECONDS,
	SHORTCUT_ROWS,
	type ModalScope
} from './shortcuts.js';

describe('actionForKey', () => {
	it('maps each transport key to its action', () => {
		expect(actionForKey('k')).toBe('toggle-play');
		expect(actionForKey('n')).toBe('next-track');
		expect(actionForKey('p')).toBe('previous-track');
		expect(actionForKey('j')).toBe('seek-back');
		expect(actionForKey('l')).toBe('seek-forward');
		expect(actionForKey('m')).toBe('toggle-mute');
		expect(actionForKey('?')).toBe('open-shortcuts');
	});

	it('matches letter keys case-insensitively so Shift does not void them', () => {
		expect(actionForKey('K')).toBe('toggle-play');
		expect(actionForKey('N')).toBe('next-track');
	});

	it('maps Escape to closing the shortcuts dialog', () => {
		expect(actionForKey('Escape')).toBe('close-shortcuts');
	});

	it('returns null for unbound and multi-character keys', () => {
		expect(actionForKey(' ')).toBeNull();
		expect(actionForKey('ArrowRight')).toBeNull();
		expect(actionForKey('Enter')).toBeNull();
		expect(actionForKey('F1')).toBeNull();
	});
});

describe('isTypingTarget', () => {
	it('treats inputs and textareas as typing targets', () => {
		expect(isTypingTarget({ tagName: 'INPUT' })).toBe(true);
		expect(isTypingTarget({ tagName: 'TEXTAREA' })).toBe(true);
	});

	it('treats contenteditable regions as typing targets', () => {
		expect(isTypingTarget({ tagName: 'DIV', isContentEditable: true })).toBe(true);
	});

	it('lets transport keys through on buttons, audio and body', () => {
		expect(isTypingTarget({ tagName: 'BUTTON' })).toBe(false);
		expect(isTypingTarget({ tagName: 'AUDIO' })).toBe(false);
		expect(isTypingTarget({ tagName: 'BODY' })).toBe(false);
	});

	it('is inert for null and non-element targets', () => {
		expect(isTypingTarget(null)).toBe(false);
		expect(isTypingTarget(undefined)).toBe(false);
		expect(isTypingTarget({})).toBe(false);
	});
});

describe('isModalOpen', () => {
	function scope(modal: unknown, roleDialogs: unknown[] = []): ModalScope {
		return {
			querySelector: () => modal,
			querySelectorAll: () => roleDialogs
		};
	}

	it('reports an open native dialog as modal', () => {
		expect(isModalOpen(scope({ tagName: 'DIALOG' }))).toBe(true);
	});

	it('reports a visible role=dialog as modal', () => {
		const visible = { getClientRects: () => [{ width: 10 }] };
		expect(isModalOpen(scope(null, [visible]))).toBe(true);
	});

	it('calls getClientRects with the dialog as its receiver', () => {
		const dialog = {
			rects: [{}] as unknown[],
			getClientRects() {
				return this.rects;
			}
		};
		const modalScope = scope(null, [dialog]);

		expect(isModalOpen(modalScope)).toBe(true);
		dialog.rects = [];
		expect(isModalOpen(modalScope)).toBe(false);
	});

	it('ignores hidden role=dialog regions', () => {
		const hidden = { getClientRects: () => [] };
		expect(isModalOpen(scope(null, [hidden]))).toBe(false);
	});

	it('reports no modal when neither signal is present', () => {
		expect(isModalOpen(scope(null, []))).toBe(false);
		expect(isModalOpen(scope(null))).toBe(false);
	});
});

describe('shortcut metadata', () => {
	it('seeks in 10-second steps', () => {
		expect(SEEK_SECONDS).toBe(10);
	});

	it('lists every binding the dialog must render', () => {
		const keys = SHORTCUT_ROWS.map((row) => row.key);
		expect(keys).toEqual(expect.arrayContaining(['k', 'n', 'p', 'j', 'l', 'm', '?']));
	});

	it('labels name the transport actions and the seek magnitude', () => {
		const labels = SHORTCUT_ROWS.map((row) => row.label).join(' | ');
		expect(labels).toMatch(/play/i);
		expect(labels).toMatch(/pause/i);
		expect(labels).toMatch(/next/i);
		expect(labels).toMatch(/prev/i);
		expect(labels).toMatch(/mute/i);
		expect(labels).toContain('10');
	});
});
