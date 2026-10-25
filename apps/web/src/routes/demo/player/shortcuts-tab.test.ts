import { expect, test, vi } from 'vitest';
import { parse } from 'svelte/compiler';
import ts from 'typescript';
import source from './+page.svelte?raw';

// Builder-owned supplement to the protected native-editing acceptance bank.
// Execute the real trap with two focusable controls so guard deletion exposes
// both extra cancellation and a focus move. Browser coverage pins DOM wiring.
const { instance } = parse(source);
if (!instance) throw new Error('Player page script missing');
const script = ts.createSourceFile(
	'player.ts', source.slice(instance.content.start, instance.content.end), ts.ScriptTarget.Latest, true
);
const trap = script.statements.find(
	(statement) => ts.isFunctionDeclaration(statement) && statement.name?.text === 'trapShortcutsTab'
);
if (!trap) throw new Error('Shortcuts Tab handler missing');
const compiled = ts.transpileModule(trap.getText(script), {
	compilerOptions: { target: ts.ScriptTarget.ES2022 }
}).outputText;

function fixture(shiftKey: boolean) {
	const document = { activeElement: null as object | null };
	const first = {
		getClientRects: () => [{}],
		focus: vi.fn(() => { document.activeElement = first; })
	};
	const last = {
		getClientRects: () => [{}],
		focus: vi.fn(() => { document.activeElement = last; })
	};
	document.activeElement = shiftKey ? first : last;
	const root = {
		querySelectorAll: () => [first, last],
		contains: (node: unknown) => node === first || node === last
	};
	const handle = new Function('shortcutsDialogEl', 'document', `${compiled}\nreturn trapShortcutsTab;`)(
		root, document
	) as (event: object) => void;
	return { handle, document, first, last };
}

for (const shiftKey of [false, true]) {
	const direction = shiftKey ? 'reverse' : 'forward';
	for (const [ownership, seed] of [
		['cancelled', { defaultPrevented: true }],
		['ctrl', { ctrlKey: true }],
		['alt', { altKey: true }],
		['meta', { metaKey: true }]
	] as const) {
		test(`KEYSAFE-TAB-U01 ${direction} ${ownership} Tab keeps prior ownership and focus`, () => {
			const { handle, document, first, last } = fixture(shiftKey);
			const active = document.activeElement;
			const event = {
				key: 'Tab', shiftKey, defaultPrevented: false,
				ctrlKey: false, altKey: false, metaKey: false, ...seed,
				preventDefault: vi.fn(() => { event.defaultPrevented = true; })
			};
			handle(event);
			expect(event.preventDefault).not.toHaveBeenCalled();
			expect(event.defaultPrevented).toBe(ownership === 'cancelled');
			expect(document.activeElement).toBe(active);
			expect(first.focus).not.toHaveBeenCalled();
			expect(last.focus).not.toHaveBeenCalled();
		});
	}

	test(`KEYSAFE-TAB-U02 unmodified ${direction} Tab still wraps focus`, () => {
		const { handle, document, first, last } = fixture(shiftKey);
		const event = {
			key: 'Tab', shiftKey, defaultPrevented: false,
			ctrlKey: false, altKey: false, metaKey: false,
			preventDefault: vi.fn(() => { event.defaultPrevented = true; })
		};
		handle(event);
		expect(event.preventDefault).toHaveBeenCalledTimes(1);
		expect(event.defaultPrevented).toBe(true);
		expect(document.activeElement).toBe(shiftKey ? last : first);
	});
}
