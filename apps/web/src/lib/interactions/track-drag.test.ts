import { afterEach, beforeEach, describe, expect, it, vi, type Mock } from 'vitest';

type Listener = (event: never) => void;

class FakeTarget {
	listeners = new Map<string, Set<Listener>>();
	addEventListener(type: string, listener: Listener) {
		if (!this.listeners.has(type)) this.listeners.set(type, new Set());
		this.listeners.get(type)!.add(listener);
	}
	removeEventListener(type: string, listener: Listener) { this.listeners.get(type)?.delete(listener); }
	fire(type: string, event: object = {}) { for (const l of [...(this.listeners.get(type) ?? [])]) l(event as never); }
	count() { return [...this.listeners.values()].reduce((n, set) => n + set.size, 0); }
}

class FakeNode {
	textContent = '';
	style = { cssText: '' };
	removed = false;
	remove() { this.removed = true; }
}

class FakeTransfer {
	data = new Map<string, string>();
	effectAllowed = '';
	dragImage: unknown;
	get types() { return [...this.data.keys()]; }
	setData(type: string, value: string) { this.data.set(type, value); }
	getData(type: string) { return this.data.get(type) ?? ''; }
	setDragImage(node: unknown) { this.dragImage = node; }
}

const TYPE = 'application/x-nntune-track-ids';
let doc: FakeTarget & { hidden: boolean; documentElement: { dataset: Record<string, string> }; body: { append: Mock }; createElement: () => FakeNode };
let win: FakeTarget;
let mod: typeof import('./track-drag.js');

function dragEvent(transfer: FakeTransfer | null = new FakeTransfer()) {
	return { dataTransfer: transfer, preventDefault: vi.fn() } as unknown as DragEvent & { preventDefault: Mock };
}
function externalEvent(data: Record<string, string>) {
	const t = new FakeTransfer();
	for (const [k, v] of Object.entries(data)) t.setData(k, v);
	return { dataTransfer: t } as unknown as DragEvent;
}

beforeEach(async () => {
	doc = Object.assign(new FakeTarget(), {
		hidden: false,
		documentElement: { dataset: {} as Record<string, string> },
		body: { append: vi.fn() },
		createElement: () => new FakeNode()
	});
	win = new FakeTarget();
	vi.stubGlobal('document', doc);
	vi.stubGlobal('window', win);
	vi.resetModules();
	mod = await import('./track-drag.js');
});
afterEach(() => {
	mod.endTrackDrag();
	vi.unstubAllGlobals();
});

describe('track drag', () => {
	it('starts a session carrying only an opaque token and dedupes ids', () => {
		const e = dragEvent();
		mod.startTrackDrag(e, ['track-1', 'track-2', 'track-1']);
		const t = e.dataTransfer as unknown as FakeTransfer;
		const token = t.getData(TYPE);
		expect(token).toBeTruthy();
		expect(token).not.toContain('track');
		expect(t.effectAllowed).toBe('copyMove');
		expect(doc.documentElement.dataset.trackDragging).toBe('library');
		expect((t.dragImage as FakeNode).textContent).toBe('2 tracks');
		expect(mod.droppedTrackDrag(e)?.ids).toEqual(['track-1', 'track-2']);
	});

	it('marks queue drags and fences them to the queue element', () => {
		const queue = {} as HTMLElement;
		const e = dragEvent();
		mod.startTrackDrag(e, ['a'], queue);
		expect(doc.documentElement.dataset.trackDragging).toBe('queue');
		expect(mod.droppedTrackDrag(e)?.queue).toBe(queue);
		expect(((e.dataTransfer as unknown as FakeTransfer).dragImage as FakeNode).textContent).toBe('1 track');
	});

	it('cancels the native drag when there is nothing to carry', () => {
		const noTransfer = dragEvent(null);
		mod.startTrackDrag(noTransfer, ['a']);
		expect(noTransfer.preventDefault).toHaveBeenCalled();
		const empty = dragEvent();
		mod.startTrackDrag(empty, []);
		expect(empty.preventDefault).toHaveBeenCalled();
		expect(doc.count()).toBe(0);
		expect(mod.activeTrackDrag(externalEvent({ [TYPE]: 'x' }))).toBeNull();
	});

	it('rejects external, missing and mismatched payloads', () => {
		const e = dragEvent();
		mod.startTrackDrag(e, ['a']);
		expect(mod.activeTrackDrag(externalEvent({ 'text/plain': 'a' }))).toBeNull();
		expect(mod.droppedTrackDrag(externalEvent({ 'text/plain': 'a' }))).toBeNull();
		expect(mod.droppedTrackDrag(externalEvent({ [TYPE]: 'forged' }))).toBeNull();
		expect(mod.droppedTrackDrag(externalEvent({ [TYPE]: '' }))).toBeNull();
		expect(mod.droppedTrackDrag({ dataTransfer: null } as unknown as DragEvent)).toBeNull();
		// Type is visible during dragover (active) even though the token is unverified.
		expect(mod.activeTrackDrag(externalEvent({ [TYPE]: 'forged' }))).not.toBeNull();
	});

	it('rejects a valid-looking token when no session is active', () => {
		const e = dragEvent();
		mod.startTrackDrag(e, ['a']);
		const token = (e.dataTransfer as unknown as FakeTransfer).getData(TYPE);
		mod.endTrackDrag();
		expect(mod.droppedTrackDrag(externalEvent({ [TYPE]: token }))).toBeNull();
	});

	it('a new drag invalidates the previous token', () => {
		const first = dragEvent();
		mod.startTrackDrag(first, ['a']);
		const second = dragEvent();
		mod.startTrackDrag(second, ['b']);
		expect(mod.droppedTrackDrag(first)).toBeNull();
		expect(mod.droppedTrackDrag(second)?.ids).toEqual(['b']);
	});

	it('ends on Escape but not on other keys', () => {
		const e = dragEvent();
		mod.startTrackDrag(e, ['a']);
		doc.fire('keydown', { key: 'Enter' });
		expect(mod.activeTrackDrag(e)).not.toBeNull();
		doc.fire('keydown', { key: 'Escape' });
		expect(mod.activeTrackDrag(e)).toBeNull();
		expect(doc.count()).toBe(0);
		expect(win.count()).toBe(0);
	});

	it.each([['document', 'dragend'], ['document', 'drop'], ['window', 'blur']])('ends on %s %s', (where, type) => {
		const e = dragEvent();
		mod.startTrackDrag(e, ['a']);
		(where === 'window' ? win : doc).fire(type);
		expect(mod.activeTrackDrag(e)).toBeNull();
	});

	it('page hiding invalidates the drag token and cleans up without requiring blur', () => {
		const e = dragEvent();
		mod.startTrackDrag(e, ['a']);
		const ghost = (e.dataTransfer as unknown as FakeTransfer).dragImage as FakeNode;
		doc.fire('visibilitychange');
		expect(mod.droppedTrackDrag(e)?.ids).toEqual(['a']);
		doc.hidden = true;
		doc.fire('visibilitychange');
		expect(mod.droppedTrackDrag(e)).toBeNull();
		expect(doc.documentElement.dataset.trackDragging).toBeUndefined();
		expect(ghost.removed).toBe(true);
		expect(doc.count()).toBe(0);
		expect(win.count()).toBe(0);
	});

	it('cleanup removes the ghost, dataset flag and all listeners', () => {
		const e = dragEvent();
		mod.startTrackDrag(e, ['a']);
		const ghost = (e.dataTransfer as unknown as FakeTransfer).dragImage as FakeNode;
		expect(doc.body.append).toHaveBeenCalledWith(ghost);
		expect(doc.count()).toBe(4);
		expect(win.count()).toBe(1);
		mod.endTrackDrag();
		expect(ghost.removed).toBe(true);
		expect(doc.documentElement.dataset.trackDragging).toBeUndefined();
		expect(doc.count()).toBe(0);
		expect(win.count()).toBe(0);
	});

	it('endTrackDrag is idempotent and restarting does not leak listeners', () => {
		mod.endTrackDrag();
		mod.endTrackDrag();
		mod.startTrackDrag(dragEvent(), ['a']);
		mod.startTrackDrag(dragEvent(), ['b']);
		expect(doc.count()).toBe(4);
		expect(win.count()).toBe(1);
	});
});
