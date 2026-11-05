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

class FakeGhost {
	textContent = '';
	style: Record<string, string> = { cssText: '' };
	offsetWidth = 40;
	offsetHeight = 20;
	removed = false;
	remove() { this.removed = true; }
}

class FakeNode extends FakeTarget {
	captured = new Set<number>();
	setPointerCapture = vi.fn((id: number) => { this.captured.add(id); });
	releasePointerCapture = vi.fn((id: number) => { this.captured.delete(id); });
	hasPointerCapture = (id: number) => this.captured.has(id);
}

let doc: FakeTarget & {
	documentElement: { dataset: Record<string, string> };
	body: { append: Mock };
	createElement: () => FakeGhost;
	elementFromPoint: Mock;
	hidden: boolean;
	scrollingElement: null;
};
let win: FakeTarget & { innerWidth: number; innerHeight: number };
let node: FakeNode;
let ghosts: FakeGhost[];
let frames: Map<number, (t: number) => void>;
let nextFrame: number;
let cancelFrame: Mock;
let inList: Set<unknown>;
let mod: typeof import('./pointer-queue-drag.js');
let opts: { ids: Mock; list: Mock; version: unknown; hover: Mock; drop: Mock; cancel: Mock };
const row = { name: 'row' };
const outside = { name: 'outside' };
const list = {
	contains: (el: unknown) => inList.has(el),
	getBoundingClientRect: () => ({ left: 0, right: 300, top: 0, bottom: 600 }),
	scrollHeight: 0,
	clientHeight: 0,
	parentElement: null
};

function ptr(over: Record<string, unknown> = {}) {
	return { pointerId: 1, pointerType: 'touch', isPrimary: true, button: 0, clientX: 10, clientY: 10, preventDefault: vi.fn(), ...over };
}
function startDrag(over: Record<string, unknown> = {}) {
	const down = ptr(over);
	node.fire('pointerdown', down);
	return down;
}
function startMoved() {
	startDrag();
	node.fire('pointermove', ptr({ clientX: 50, clientY: 60 }));
}
function mount(version: unknown = 1) {
	opts = { ids: vi.fn(() => ['a', 'b']), list: vi.fn(() => list), version, hover: vi.fn(), drop: vi.fn(), cancel: vi.fn() };
	return mod.pointerQueueDrag(node as unknown as HTMLElement, opts);
}

beforeEach(async () => {
	ghosts = [];
	frames = new Map();
	nextFrame = 1;
	inList = new Set([row]);
	cancelFrame = vi.fn((id: number) => { frames.delete(id); });
	node = new FakeNode();
	doc = Object.assign(new FakeTarget(), {
		documentElement: { dataset: {} as Record<string, string> },
		body: { append: vi.fn() },
		createElement: () => { const g = new FakeGhost(); ghosts.push(g); return g; },
		elementFromPoint: vi.fn((): unknown => row),
		hidden: false,
		scrollingElement: null
	});
	win = Object.assign(new FakeTarget(), { innerWidth: 400, innerHeight: 800 });
	vi.stubGlobal('document', doc);
	vi.stubGlobal('window', win);
	vi.stubGlobal('requestAnimationFrame', (cb: (t: number) => void) => { frames.set(nextFrame, cb); return nextFrame++; });
	vi.stubGlobal('cancelAnimationFrame', cancelFrame);
	vi.stubGlobal('getComputedStyle', () => ({ overflowY: 'visible' }));
	vi.resetModules();
	mod = await import('./pointer-queue-drag.js');
});
afterEach(() => vi.unstubAllGlobals());

describe('pointer queue drag', () => {
	it('captures a primary touch or pen press on the grip and prevents default', () => {
		mount();
		const touch = startDrag();
		expect(touch.preventDefault).toHaveBeenCalled();
		expect(node.setPointerCapture).toHaveBeenCalledWith(1);
		expect(win.listeners.get('blur')?.size).toBe(1);
		expect(doc.listeners.get('keydown')?.size).toBe(1);
		expect(doc.listeners.get('visibilitychange')?.size).toBe(1);
		node.fire('pointerup', ptr());
		startDrag({ pointerType: 'pen', pointerId: 2 });
		expect(node.setPointerCapture).toHaveBeenLastCalledWith(2);
	});

	it.each([
		['mouse', { pointerType: 'mouse' }],
		['non-primary', { isPrimary: false }],
		['non-main button', { button: 2 }]
	])('ignores %s pointerdown', (_name, over) => {
		mount();
		const e = startDrag(over);
		expect(e.preventDefault).not.toHaveBeenCalled();
		expect(node.setPointerCapture).not.toHaveBeenCalled();
		expect(doc.count()).toBe(0);
		expect(win.count()).toBe(0);
	});

	it('does not start with no ids or no list', () => {
		mount();
		opts.ids.mockReturnValue([]);
		expect(startDrag().preventDefault).not.toHaveBeenCalled();
		opts.ids.mockReturnValue(['a']);
		opts.list.mockReturnValue(null);
		expect(startDrag().preventDefault).not.toHaveBeenCalled();
		expect(node.setPointerCapture).not.toHaveBeenCalled();
	});

	it('shows no ghost until movement reaches the 6px threshold', () => {
		mount();
		startDrag();
		const small = ptr({ clientX: 14, clientY: 14 }); // ~5.66px
		node.fire('pointermove', small);
		expect(small.preventDefault).not.toHaveBeenCalled();
		expect(ghosts).toHaveLength(0);
		expect(opts.hover).not.toHaveBeenCalled();
		node.fire('pointermove', ptr({ clientX: 16, clientY: 10 }));
		expect(ghosts).toHaveLength(1);
		expect(ghosts[0].textContent).toBe('2 tracks');
		expect(doc.documentElement.dataset.trackDragging).toBe('queue');
		expect(doc.body.append).toHaveBeenCalledWith(ghosts[0]);
		expect(frames.size).toBe(1);
	});

	it('uses singular ghost text and clamps its position to the viewport', () => {
		mount();
		opts.ids.mockReturnValue(['a']);
		startDrag();
		node.fire('pointermove', ptr({ clientX: 395, clientY: 795 }));
		expect(ghosts[0].textContent).toBe('1 track');
		expect(ghosts[0].style.left).toBe('360px');
		expect(ghosts[0].style.top).toBe('780px');
	});

	it('ignores moves from other pointers', () => {
		mount();
		startDrag();
		node.fire('pointermove', ptr({ pointerId: 9, clientX: 100, clientY: 100 }));
		expect(ghosts).toHaveLength(0);
	});

	it('hovers a target inside the list and cancels outside it', () => {
		mount();
		startMoved();
		expect(opts.hover).toHaveBeenCalledWith(row, 60);
		opts.cancel.mockClear();
		doc.elementFromPoint.mockReturnValue(outside);
		node.fire('pointermove', ptr({ clientX: 60, clientY: 70 }));
		expect(opts.cancel).toHaveBeenCalledTimes(1);
		doc.elementFromPoint.mockReturnValue(null);
		node.fire('pointermove', ptr({ clientX: 61, clientY: 70 }));
		expect(opts.cancel).toHaveBeenCalledTimes(2);
	});

	it('the animation frame loop keeps hovering and schedules the next frame', () => {
		mount();
		startMoved();
		opts.hover.mockClear();
		const [[id, cb]] = [...frames];
		frames.delete(id);
		cb(100);
		expect(opts.hover).toHaveBeenCalledWith(row, 60);
		expect(frames.size).toBe(1);
	});

	it('drops the dragged ids on a valid target, then clears state', () => {
		mount();
		startMoved();
		node.fire('pointerup', ptr({ clientX: 55, clientY: 65 }));
		expect(opts.drop).toHaveBeenCalledWith(['a', 'b'], row, 65);
		expect(ghosts[0].removed).toBe(true);
		expect(node.setPointerCapture).toHaveBeenCalled();
		expect(node.releasePointerCapture).toHaveBeenCalledWith(1);
		expect(doc.documentElement.dataset.trackDragging).toBeUndefined();
		expect(doc.count()).toBe(0);
		expect(win.count()).toBe(0);
	});

	it('does not drop outside the list', () => {
		mount();
		startMoved();
		doc.elementFromPoint.mockReturnValue(outside);
		node.fire('pointerup', ptr());
		expect(opts.drop).not.toHaveBeenCalled();
		expect(opts.cancel).toHaveBeenCalled();
		expect(ghosts[0].removed).toBe(true);
	});

	it('does not drop a tap that never passed the threshold', () => {
		mount();
		startDrag();
		node.fire('pointerup', ptr());
		expect(opts.drop).not.toHaveBeenCalled();
		expect(node.releasePointerCapture).toHaveBeenCalledWith(1);
	});

	it('ignores pointerup from another pointer', () => {
		mount();
		startMoved();
		node.fire('pointerup', ptr({ pointerId: 7 }));
		expect(opts.drop).not.toHaveBeenCalled();
		expect(ghosts[0].removed).toBe(false);
	});

	function expectAborted() {
		expect(opts.drop).not.toHaveBeenCalled();
		expect(opts.cancel).toHaveBeenCalled();
		expect(ghosts[0].removed).toBe(true);
		expect(cancelFrame).toHaveBeenCalled();
		expect(frames.size).toBe(0);
		expect(node.captured.size).toBe(0);
		expect(doc.documentElement.dataset.trackDragging).toBeUndefined();
		expect(doc.count()).toBe(0);
		expect(win.count()).toBe(0);
	}

	it('pointercancel aborts the drag', () => {
		mount();
		startMoved();
		node.fire('pointercancel', ptr());
		expectAborted();
	});

	it('lostpointercapture aborts the drag', () => {
		mount();
		startMoved();
		node.fire('lostpointercapture', ptr());
		expectAborted();
	});

	it('ignores pointercancel from another pointer', () => {
		mount();
		startMoved();
		node.fire('pointercancel', ptr({ pointerId: 5 }));
		expect(ghosts[0].removed).toBe(false);
	});

	it('Escape aborts the drag and prevents default; other keys do not', () => {
		mount();
		startMoved();
		const other = { key: 'a', preventDefault: vi.fn() };
		doc.fire('keydown', other);
		expect(ghosts[0].removed).toBe(false);
		const esc = { key: 'Escape', preventDefault: vi.fn() };
		doc.fire('keydown', esc);
		expect(esc.preventDefault).toHaveBeenCalled();
		expectAborted();
	});

	it('window blur aborts the drag', () => {
		mount();
		startMoved();
		win.fire('blur');
		expectAborted();
	});

	it('visibilitychange aborts only when the document is hidden', () => {
		mount();
		startMoved();
		doc.fire('visibilitychange');
		expect(ghosts[0].removed).toBe(false);
		doc.hidden = true;
		doc.fire('visibilitychange');
		expectAborted();
	});

	it('a new pointerdown finishes a prior drag first', () => {
		mount();
		startMoved();
		startDrag({ pointerId: 3 });
		expect(ghosts[0].removed).toBe(true);
		expect(node.setPointerCapture).toHaveBeenLastCalledWith(3);
	});

	it('replacing the version cancels an active drag; the same version does not', () => {
		const action = mount(1);
		startMoved();
		const same = { ...opts, version: 1, cancel: vi.fn() };
		action.update(same);
		expect(ghosts[0].removed).toBe(false);
		const next = { ...opts, version: 2, cancel: vi.fn(), hover: vi.fn(), drop: vi.fn() };
		action.update(next);
		expect(ghosts[0].removed).toBe(true);
		expect(same.cancel).toHaveBeenCalled(); // finish ran with the old options
		node.fire('pointerup', ptr());
		expect(next.drop).not.toHaveBeenCalled();
		// New options are used for later drags.
		startMoved();
		expect(next.hover).toHaveBeenCalled();
	});

	it('destroy mid-drag removes every listener, frame, ghost and capture', () => {
		const action = mount();
		startMoved();
		action.destroy();
		expectAborted();
		expect(node.count()).toBe(0);
		node.fire('pointerdown', ptr());
		expect(node.setPointerCapture).toHaveBeenCalledTimes(1);
	});

	it('destroy while idle removes the node listeners', () => {
		const action = mount();
		expect(node.count()).toBe(5);
		action.destroy();
		expect(node.count()).toBe(0);
		expect(opts.cancel).not.toHaveBeenCalled();
	});
});
