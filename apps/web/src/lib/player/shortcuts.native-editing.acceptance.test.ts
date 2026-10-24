import { describe, expect, it } from 'vitest';
import { actionForKey, isTypingTarget } from './shortcuts.js';

/**
 * Protected native-editing acceptance. No new production helper is required:
 * isTypingTarget(unknown): boolean remains the context-classification seam.
 * Event cancellation/composition and media effects are exercised through the
 * real demo route in player-shortcuts-native-editing.spec.ts, not a duplicate
 * key handler here. Real DOM inheritance is covered there; these are structural
 * unit inputs, like the neighboring shortcuts.test.ts.
 *
 * Breaks caught: forgetting SELECT/OPTION; narrowing INPUT to text-only types;
 * dropping inherited isContentEditable; overblocking ordinary buttons/audio.
 */
describe('native editing shortcut contexts', () => {
	it.each(['SELECT', 'OPTION'])('KEYSAFE-U01 %s retains native key ownership', (tagName) => {
		expect(isTypingTarget({ tagName }), `${tagName}: native editing must win`).toBe(true);
	});

	it.each(['text', 'search', 'number', 'range', 'checkbox', 'radio', 'date'])(
		'KEYSAFE-U02 input type=%s retains native key ownership',
		(type) => {
			expect(isTypingTarget({ tagName: 'INPUT', type })).toBe(true);
		}
	);

	it('KEYSAFE-U03 textarea and inherited editable descendants retain native key ownership', () => {
		expect(isTypingTarget({ tagName: 'TEXTAREA' })).toBe(true);
		// isContentEditable is inherited by a span inside a real editing host.
		expect(isTypingTarget({ tagName: 'SPAN', isContentEditable: true })).toBe(true);
	});

	it.each(['BUTTON', 'AUDIO', 'BODY'])('KEYSAFE-U04 %s still permits player shortcuts', (tagName) => {
		expect(isTypingTarget({ tagName, isContentEditable: false })).toBe(false);
	});

	it('KEYSAFE-U05 non-elements remain harmless and the original key map stays intact', () => {
		for (const target of [null, undefined, {}, { tagName: 'DIV', isContentEditable: false }]) {
			expect(isTypingTarget(target)).toBe(false);
		}
		for (const [key, action] of [
			['k', 'toggle-play'], ['n', 'next-track'], ['p', 'previous-track'],
			['j', 'seek-back'], ['l', 'seek-forward'], ['m', 'toggle-mute'],
			['?', 'open-shortcuts'], ['Escape', 'close-shortcuts'], ['K', 'toggle-play']
		]) expect(actionForKey(key)).toBe(action);
		for (const key of [' ', 'ArrowLeft', 'ArrowRight', 'Enter']) expect(actionForKey(key)).toBeNull();
	});
});
