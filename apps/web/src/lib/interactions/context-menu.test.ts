import { afterEach, beforeEach, describe, expect, it, vi, type Mock } from 'vitest';
import { contextMenu, inlineSingleActions, menuAt, menuPosition } from './context-menu.js';

type Listener = (event: never) => void;

class FakeElement {
	listeners = new Map<string, Set<Listener>>();
	matches = false;
	rect = { left: 12, bottom: 34 };
	addEventListener(type: string, listener: Listener) {
		if (!this.listeners.has(type)) this.listeners.set(type, new Set());
		this.listeners.get(type)!.add(listener);
	}
	removeEventListener(type: string, listener: Listener) { this.listeners.get(type)?.delete(listener); }
	longpress = false;
	closest(selector: string) { return (selector === '[data-context-menu-longpress]' ? this.longpress : this.matches) ? this : null; }
	getBoundingClientRect() { return this.rect; }
	count() { return [...this.listeners.values()].reduce((n, set) => n + set.size, 0); }
	fire(type: string, event: Record<string, unknown> = {}) {
		const full = { preventDefault: vi.fn(), stopPropagation: vi.fn(), target: this, ...event };
		for (const listener of this.listeners.get(type) ?? []) listener(full as never);
		return full as typeof full & { preventDefault: Mock; stopPropagation: Mock };
	}
}
class FakeHTMLElement extends FakeElement {}

describe('inlineSingleActions', () => {
	it('flattens nested single choices but retains real multi-choice groups', () => {
		const run = vi.fn();
		const leaf = { label: 'Copy link', run };
		const group = { label: 'Destinations', children: [leaf, { label: 'Other', run }] };
		expect(inlineSingleActions([{ label: 'Share', children: [{ label: 'Link', children: [leaf] }] }, group]))
			.toEqual([{ ...leaf, disabled: undefined }, group]);
	});
	it('never enables the child of a disabled group', () => {
		expect(inlineSingleActions([{ label: 'Unavailable', disabled: true, children: [{ label: 'Child', run: vi.fn() }] }])[0].disabled).toBe(true);
	});
});

describe('menuPosition', () => {
	it('keeps an in-bounds origin unchanged', () => {
		expect(menuPosition(10, 20, 100, 50, 800, 600)).toEqual({ left: 10, top: 20 });
	});
	it('flips above the origin when it would overflow the bottom', () => {
		expect(menuPosition(10, 580, 100, 50, 800, 600)).toEqual({ left: 10, top: 530 });
	});
	it('clamps to the right edge', () => {
		expect(menuPosition(790, 20, 100, 50, 800, 600).left).toBe(700);
	});
	it('clamps negative coordinates and oversized menus to zero', () => {
		expect(menuPosition(-5, -5, 100, 50, 800, 600)).toEqual({ left: 0, top: 0 });
		expect(menuPosition(10, 10, 900, 700, 800, 600)).toEqual({ left: 0, top: 0 });
	});
	it('clamps to the top when the flipped origin is above the viewport', () => {
		expect(menuPosition(0, 40, 10, 100, 800, 120).top).toBe(0);
	});
});

describe('menuAt', () => {
	it('anchors to the invoker bottom-left', () => {
		const el = new FakeHTMLElement();
		const request = menuAt(el as never, 'Track', []);
		expect(request).toMatchObject({ label: 'Track', x: 12, y: 34, invoker: el });
	});
});

describe('contextMenu action', () => {
	let node: FakeHTMLElement;
	let doc: FakeElement & { hidden: boolean };
	let win: FakeElement;
	let open: Mock<(invoker: unknown, point?: unknown) => void>;
	beforeEach(() => {
		vi.useFakeTimers();
		vi.stubGlobal('Element', FakeElement);
		vi.stubGlobal('HTMLElement', FakeHTMLElement);
		node = new FakeHTMLElement();
		doc = Object.assign(new FakeElement(), { hidden: false });
		win = new FakeElement();
		vi.stubGlobal('document', doc);
		vi.stubGlobal('window', win);
		open = vi.fn();
	});
	afterEach(() => {
		vi.useRealTimers();
		vi.unstubAllGlobals();
	});
	const touch = (extra: Record<string, unknown> = {}) => ({ pointerType: 'touch', isPrimary: true, button: 0, clientX: 5, clientY: 6, ...extra });

	it('allows primary row controls to opt into long press without swallowing a short tap', () => {
		contextMenu(node as never, open);
		const control = new FakeHTMLElement();
		control.matches = true;
		control.longpress = true;
		node.fire('pointerdown', touch({ target: control }));
		node.fire('pointerup');
		vi.advanceTimersByTime(500);
		expect(open).not.toHaveBeenCalled();
		expect(node.fire('click').preventDefault).not.toHaveBeenCalled();
		node.fire('pointerdown', touch({ target: control }));
		vi.advanceTimersByTime(500);
		expect(open).toHaveBeenCalledOnce();
	});

	it('opens on right click at the pointer and suppresses the native menu', () => {
		contextMenu(node as never, open);
		const e = node.fire('contextmenu', { clientX: 3, clientY: 4 });
		expect(e.preventDefault).toHaveBeenCalled();
		expect(e.stopPropagation).toHaveBeenCalled();
		expect(open).toHaveBeenCalledWith(node, { x: 3, y: 4 });
	});

	it('opens on ContextMenu and Shift+F10 but ignores other keys', () => {
		contextMenu(node as never, open);
		node.fire('keydown', { key: 'ContextMenu' });
		node.fire('keydown', { key: 'F10', shiftKey: true });
		node.fire('keydown', { key: 'F10', shiftKey: false });
		node.fire('keydown', { key: 'a' });
		expect(open).toHaveBeenCalledTimes(2);
		expect(open).toHaveBeenCalledWith(node);
	});

	it('uses a focused HTMLElement target as the keyboard invoker', () => {
		contextMenu(node as never, open);
		const child = new FakeHTMLElement();
		node.fire('keydown', { key: 'ContextMenu', target: child });
		expect(open).toHaveBeenCalledWith(child);
	});

	it('long-press opens after 500 ms and swallows the following click once', () => {
		contextMenu(node as never, open);
		node.fire('pointerdown', touch());
		vi.advanceTimersByTime(499);
		expect(open).not.toHaveBeenCalled();
		vi.advanceTimersByTime(1);
		expect(open).toHaveBeenCalledWith(node, { x: 5, y: 6 });
		expect(node.fire('mousedown').preventDefault).toHaveBeenCalled();
		const first = node.fire('click');
		expect(first.preventDefault).toHaveBeenCalled();
		const second = node.fire('click');
		expect(second.preventDefault).not.toHaveBeenCalled();
	});

	it('canceled long-press never suppresses a later keyboard activation', () => {
		const action = contextMenu(node as never, open);
		node.fire('pointerdown', touch());
		vi.advanceTimersByTime(500);
		node.fire('pointercancel');
		expect(open).toHaveBeenCalledOnce();
		expect(node.fire('click', { detail: 0 }).preventDefault).not.toHaveBeenCalled();
		expect(node.fire('click', { detail: 1 }).preventDefault).not.toHaveBeenCalled();
		action.destroy();
	});

	it('suppresses only a compatibility click, not keyboard activation after a long press', () => {
		const action = contextMenu(node as never, open);
		node.fire('pointerdown', touch());
		vi.advanceTimersByTime(500);
		node.fire('pointerup');
		expect(node.fire('click', { detail: 0 }).preventDefault).not.toHaveBeenCalled();
		action.destroy();
	});

	it('cancels long-press on pointerup, large move and dragstart; tolerates small jitter', () => {
		contextMenu(node as never, open);
		node.fire('pointerdown', touch());
		node.fire('pointerup');
		vi.advanceTimersByTime(600);
		node.fire('pointerdown', touch());
		node.fire('pointermove', { clientX: 5, clientY: 30 });
		vi.advanceTimersByTime(600);
		node.fire('pointerdown', touch());
		node.fire('dragstart');
		vi.advanceTimersByTime(600);
		expect(open).not.toHaveBeenCalled();
		node.fire('pointerdown', touch());
		node.fire('pointermove', { clientX: 8, clientY: 8 });
		vi.advanceTimersByTime(500);
		expect(open).toHaveBeenCalledTimes(1);
	});

	it.each(['blur', 'scroll', 'pointerdown', 'pointerup', 'pointercancel', 'hidden'])('cancels a pending press on external %s', (type) => {
		const action = contextMenu(node as never, open);
		node.fire('pointerdown', touch());
		expect(doc.count()).toBe(5);
		expect(win.count()).toBe(1);
		vi.advanceTimersByTime(250);
		if (type === 'hidden') { doc.hidden = true; doc.fire('visibilitychange'); }
		else (type === 'blur' ? win : doc).fire(type);
		vi.advanceTimersByTime(500);
		expect(open).not.toHaveBeenCalled();
		expect(node.fire('click').preventDefault).not.toHaveBeenCalled();
		expect(doc.count()).toBe(0);
		expect(win.count()).toBe(0);
		action.destroy();
	});

	it('retains a pending press on a visible visibility event and cleans up after opening', () => {
		const action = contextMenu(node as never, open);
		node.fire('pointerdown', touch());
		doc.fire('visibilitychange');
		vi.advanceTimersByTime(500);
		expect(open).toHaveBeenCalledOnce();
		expect(doc.count()).toBe(0);
		expect(win.count()).toBe(0);
		action.destroy();
	});

	it('a secondary touch cancels the first press instead of starting another', () => {
		const action = contextMenu(node as never, open);
		node.fire('pointerdown', touch());
		vi.advanceTimersByTime(250);
		node.fire('pointerdown', touch({ isPrimary: false }));
		vi.advanceTimersByTime(500);
		expect(open).not.toHaveBeenCalled();
		expect(doc.count()).toBe(0);
		expect(win.count()).toBe(0);
		action.destroy();
	});

	it('a secondary touch after opening preserves one compatibility-click suppression', () => {
		const action = contextMenu(node as never, open);
		node.fire('pointerdown', touch());
		vi.advanceTimersByTime(500);
		doc.fire('pointerdown', touch({ isPrimary: false }));
		node.fire('pointerdown', touch({ isPrimary: false }));
		node.fire('pointerup');
		vi.advanceTimersByTime(500);
		expect(open).toHaveBeenCalledOnce();
		expect(node.fire('mousedown').preventDefault).toHaveBeenCalled();
		expect(node.fire('click', { detail: 1 }).preventDefault).toHaveBeenCalled();
		expect(node.fire('click', { detail: 1 }).preventDefault).not.toHaveBeenCalled();
		action.destroy();
	});

	it('does not long-press for mouse, non-primary button or native controls', () => {
		contextMenu(node as never, open);
		node.fire('pointerdown', touch({ pointerType: 'mouse' }));
		node.fire('pointerdown', touch({ button: 2 }));
		const control = new FakeElement();
		control.matches = true;
		node.fire('pointerdown', touch({ target: control }));
		vi.advanceTimersByTime(1000);
		expect(open).not.toHaveBeenCalled();
	});

	it('update swaps the callback', () => {
		const action = contextMenu(node as never, open);
		const next = vi.fn();
		action.update(next);
		node.fire('contextmenu', { clientX: 1, clientY: 2 });
		expect(open).not.toHaveBeenCalled();
		expect(next).toHaveBeenCalled();
	});

	it('destroy removes every listener and clears a pending timer', () => {
		const action = contextMenu(node as never, open);
		expect(node.count()).toBe(9);
		node.fire('pointerdown', touch());
		action.destroy();
		expect(node.count()).toBe(0);
		vi.advanceTimersByTime(1000);
		expect(open).not.toHaveBeenCalled();
		expect(vi.getTimerCount()).toBe(0);
	});
});
