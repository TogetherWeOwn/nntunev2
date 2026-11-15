/** Builder-owned rendering and extracted-handler checks, not browser/focus evidence. */
import { afterEach, expect, it, vi } from 'vitest';
import { render } from 'svelte/server';
import ts from 'typescript';
import PlayerQueue from './PlayerQueue.svelte';
import { createPlayerSession } from './session.js';
import { createQueue, currentTrack } from './queue.js';

const sources = import.meta.glob('./PlayerQueue.svelte', { query: '?raw', import: 'default', eager: true }) as Record<string, string>;
const sessions: ReturnType<typeof createPlayerSession>[] = [];
const tracks = ['a', 'b', 'c', 'd'].map((id) => ({ id, title: id.toUpperCase(), artist: '', streamUrl: '' }));

function setup() {
	const player = createPlayerSession({ flush: async () => {} });
	sessions.push(player);
	player.start(null);
	player.setQueue(createQueue(tracks));
	const body = (enabled = true) => render(PlayerQueue, { props: {
		state: player.snapshot().queue,
		filterQuery: player.snapshot().queueFilterQuery,
		...(enabled ? { selection: player.snapshot().queueSelection, selectionControls: player } : {})
	} }).body;
	return { player, body };
}

afterEach(() => {
	for (const player of sessions.splice(0)) player.destroy();
	vi.unstubAllGlobals();
});

it.each([0, 2])('one session-owned group contains the sole exact removal control with %i selected', (count) => {
	const { player } = setup();
	player.toggleQueueSelection('b');
	if (count === 0) player.toggleQueueSelection('b');
	else player.toggleQueueSelection('d');
	const props = () => ({ state: player.snapshot().queue, selection: player.snapshot().queueSelection,
		selectionControls: player, onreorder: vi.fn() });
	const markup = render(PlayerQueue, { props: props() }).body;
	expect(markup.match(/data-testid="queue-selection-toolbar"/g)).toHaveLength(1);
	expect(markup.match(/aria-label="Selected queue tracks"/g)).toHaveLength(1);
	const group = markup.match(/<div[^>]*role="group"[^>]*data-testid="queue-selection-toolbar"[^>]*>([\s\S]*?)<\/div>/)![1];
	expect(group).toContain(`>${count} selected in queue</p>`);
	expect(group.match(/>Remove selected from queue<\/button>/g)).toHaveLength(1);
	const button = group.match(/<button[^>]*>Remove selected from queue<\/button>/)![0];
	expect(button.includes('disabled')).toBe(count === 0);
	expect(button).not.toMatch(/aria-label(?:ledby)?=|title=/);
	const buttons = [...group.matchAll(/<button\b[^>]*>([^<]*)<\/button>/g)].map((match) => match[1]);
	expect(buttons.slice(-2)).toEqual(['Remove selected from queue', 'Done']);
	expect(group).toContain('Move selected before');
	expect(group).toContain('>Move selected</button>');
	player.clearQueueSelection();
	expect(render(PlayerQueue, { props: props() }).body).not.toContain('data-testid="queue-selection-toolbar"');
});

it('selection is opt-in; production selectors do not replace existing direct Select controls or current marking', () => {
	const { player, body } = setup();
	expect(body(false)).not.toContain('type="checkbox"');
	const markup = render(PlayerQueue, { props: {
		state: player.snapshot().queue, selection: player.snapshot().queueSelection,
		selectionControls: player, onselect: () => {}
	} }).body;
	expect(markup.match(/type="checkbox"/g)).toHaveLength(4);
	expect(markup).toContain('aria-label="Select A for queue actions"');
	expect(markup).toContain('aria-label="Select A"');
	expect(markup.match(/aria-current="true"/g)).toHaveLength(1);
	expect(markup).not.toContain('data-testid="queue-selection-toolbar"');
});

it('the bulk bar includes hidden membership, preserves filter scope and renders the native checked state', () => {
	const { player, body } = setup();
	player.toggleQueueSelection('b');
	player.toggleQueueSelection('d');
	player.setQueueFilter('b');
	const markup = body();
	expect(markup).toContain('data-testid="queue-selection-toolbar"');
	expect(markup).toContain('>2 selected in queue</p>');
	expect(markup).toContain('1 selected track is hidden by this filter.');
	expect(markup.match(/type="checkbox"[^>]*checked/g)).toHaveLength(1);
	expect(markup).toContain('>Play next</button>');
	expect(markup).toContain('>Remove selected from queue</button>');
	expect(markup).toContain('>Done</button>');
	expect(player.snapshot().queue.tracks).toEqual(tracks);
});

it('an active empty selection disables bulk edits and a zero-match projection disables Select all displayed', () => {
	const { player, body } = setup();
	player.toggleQueueSelection('b');
	player.toggleQueueSelection('b');
	player.setQueueFilter('no match');
	const markup = body();
	for (const label of ['Play next', 'Remove selected from queue', 'Select all displayed']) {
		expect(markup).toMatch(new RegExp(`<button[^>]*disabled[^>]*>${label}</button>`));
	}
	expect(markup).toContain('>Done</button>');
});

it.each([
	['remove', ['b', 'd'], ['a', 'c'], 'Removed 2 tracks from the queue.'],
	['remove', ['d'], ['a', 'b', 'c'], 'Removed 1 track from the queue.'],
	['playNext', ['a', 'd'], ['a', 'd', 'b', 'c'], 'Moved 1 track to play next.'],
	['playNext', ['c', 'd'], ['a', 'c', 'd', 'b'], 'Moved 2 tracks to play next.']
] as const)('%s announces the exact committed count, including hidden selection and excluding current from Play next', (action, ids, expected, feedback) => {
	const { player } = setup();
	ids.forEach((id) => player.toggleQueueSelection(id));
	player.setQueueFilter('b');
	const before = player.snapshot();
	expect(player.editQueueSelection(action)).toBe(true);
	expect(player.snapshot().queue.tracks.map((track) => track.id)).toEqual(expected);
	expect(currentTrack(player.snapshot().queue)?.id).toBe('a');
	expect(player.snapshot().queueFeedback).toBe(feedback);
	expect(player.snapshot().queueFeedbackVersion).toBe(before.queueFeedbackVersion + 1);
	expect(player.snapshot().queueFilterQuery).toBe('b');
});

it('a subscriber changing membership during the committed queue notification does not rewrite its outcome count', () => {
	const { player } = setup();
	player.toggleQueueSelection('d');
	const before = player.snapshot().queue;
	const stop = player.subscribe((snapshot) => {
		if (snapshot.queue !== before && snapshot.queueSelection.ids.length) player.clearQueueSelection();
	});
	expect(player.editQueueSelection('playNext')).toBe(true);
	expect(player.snapshot().queueFeedback).toBe('Moved 1 track to play next.');
	stop();
});

it.each(['resetAtAuthBoundary', 'destroy'] as const)('%s during the queue notification prevents any bulk outcome from crossing the boundary', (boundary) => {
	const { player } = setup();
	player.toggleQueueSelection('d');
	const before = player.snapshot().queue;
	let armed = true;
	const stop = player.subscribe((snapshot) => {
		if (armed && snapshot.queue !== before) { armed = false; player[boundary](); }
	});
	expect(player.editQueueSelection('playNext')).toBe(false);
	expect(player.snapshot().queue.tracks).toEqual([]);
	expect(player.snapshot().queueFeedback).toBe('');
	stop();
});

it('a no-op bulk action neither repeats an outcome nor clears a prior result', () => {
	const { player } = setup();
	player.toggleQueueSelection('b');
	player.setQueueFeedback('Retained result');
	const before = player.snapshot();
	expect(player.editQueueSelection('playNext')).toBe(false);
	expect(player.snapshot()).toBe(before);
});

it('a returned selection reflects subscriber-triggered auth cleanup instead of a stale checkbox toggle', () => {
	const { player } = setup();
	let armed = true;
	const stop = player.subscribe((snapshot) => {
		if (armed && snapshot.queueSelection.active) { armed = false; player.resetAtAuthBoundary(); }
	});
	expect(player.toggleQueueSelection('b').ids).toEqual([]);
	expect(player.snapshot().queueSelection.ids).toEqual([]);
	stop();
});

class Target {
	dataset: { trackId?: string };
	constructor(public kind = 'row', public id = 'b', public inside = true) { this.dataset = { trackId: id }; }
	closest(selector: string) {
		if (selector === '[data-testid="queue-item"]') return new Target('row', this.id, this.inside);
		const control = selector.includes('button')
			? ['button', 'input', 'textarea', 'select', 'a', 'label', 'editable', 'checkbox']
			: ['textarea', 'select', 'editable', 'input'];
		return control.includes(this.kind) ? this : null;
	}
	matches() { return this.kind === 'row'; }
}

function extracted(names: string[]) {
	const source = sources['./PlayerQueue.svelte'];
	const script = source.slice(source.indexOf('>') + 1, source.indexOf('</script'));
	const ast = ts.createSourceFile('queue.ts', script, ts.ScriptTarget.Latest, true);
	const handlers = ast.statements.filter((node) => ts.isFunctionDeclaration(node) && node.name &&
		names.includes(node.name.text)).map((node) => node.getText(ast)).join('\n');
	return ts.transpileModule(handlers, { compilerOptions: { target: ts.ScriptTarget.ES2022 } }).outputText;
}

function handlers(player: ReturnType<typeof createPlayerSession>) {
	const compiled = extracted(['selectionClick', 'selectionKey']);
	const list = { contains: (node: Target) => node.inside };
	return new Function('selectionControls', 'list', 'Element', 'HTMLElement', `
		let selection;
		${compiled}
		return (kind, event) => {
			selection = selectionControls.snapshot().queueSelection;
			return kind === 'click' ? selectionClick(event) : selectionKey(event);
		};
	`)(player, list, Target, Target) as (kind: string, event: object) => void;
}

function event(overrides = {}) {
	return { target: new Target(), key: ' ', ctrlKey: false, metaKey: false, shiftKey: false,
		altKey: false, defaultPrevented: false, repeat: false, isComposing: false,
		preventDefault: vi.fn(), ...overrides };
}

it.each(['ctrlKey', 'metaKey'] as const)('%s row click toggles membership without direct selection or queue edits', (modifier) => {
	const { player } = setup();
	const before = player.snapshot().queue;
	const run = handlers(player);
	const click = event({ [modifier]: true });
	run('click', click);
	expect(player.snapshot().queueSelection.ids).toEqual(['b']);
	expect(click.preventDefault).toHaveBeenCalledOnce();
	run('click', click);
	expect(player.snapshot().queueSelection.ids).toEqual([]);
	expect(player.snapshot().queue).toBe(before);
});

it('Shift uses the arranged filtered range and keeps hidden membership', () => {
	const { player } = setup();
	const run = handlers(player);
	player.toggleQueueSelection('a');
	player.setQueueFilter('b');
	run('click', event({ shiftKey: true }));
	expect(player.snapshot().queueSelection.ids).toEqual(['a', 'b']);
	player.setQueueFilter('');
	run('click', event({ target: new Target('row', 'd'), shiftKey: true }));
	expect(player.snapshot().queueSelection.ids).toEqual(['a', 'b', 'c', 'd']);
});

it.each(['button', 'input', 'textarea', 'select', 'a', 'label', 'editable', 'checkbox'])('modifier clicks on %s keep the native action', (kind) => {
	const { player } = setup();
	const click = event({ target: new Target(kind), ctrlKey: true });
	handlers(player)('click', click);
	expect(player.snapshot().queueSelection.ids).toEqual([]);
	expect(click.preventDefault).not.toHaveBeenCalled();
});

it.each([{ altKey: true, ctrlKey: true }, { defaultPrevented: true, ctrlKey: true }, {},
	{ ctrlKey: true, target: new Target('row', 'b', false) }])('unowned row clicks do nothing: %j', (flags) => {
	const { player } = setup();
	const click = event(flags);
	handlers(player)('click', click);
	expect(player.snapshot().queueSelection.ids).toEqual([]);
	expect(click.preventDefault).not.toHaveBeenCalled();
});

it.each(['ctrlKey', 'metaKey'] as const)('%s+A selects displayed rows additively, from row or checkbox focus', (modifier) => {
	const { player } = setup();
	player.toggleQueueSelection('a');
	player.setQueueFilter('b');
	const key = event({ key: 'A', [modifier]: true, target: new Target('checkbox') });
	handlers(player)('key', key);
	expect(player.snapshot().queueSelection.ids).toEqual(['a', 'b']);
	expect(key.preventDefault).toHaveBeenCalledOnce();
});

it.each([{ repeat: true }, { isComposing: true }, { altKey: true }, { defaultPrevented: true },
	...['input', 'textarea', 'select', 'editable'].map((kind) => ({ target: new Target(kind) }))])('Select All does not consume guarded input: %j', (flags) => {
	const { player } = setup();
	const key = event({ key: 'a', ctrlKey: true, ...flags });
	handlers(player)('key', key);
	expect(player.snapshot().queueSelection.ids).toEqual([]);
	expect(key.preventDefault).not.toHaveBeenCalled();
});

it('Space toggles only the focused row in active mode; native buttons/checkboxes own their Space', () => {
	const { player } = setup();
	const run = handlers(player);
	const inactive = event();
	run('key', inactive);
	expect(inactive.preventDefault).not.toHaveBeenCalled();
	player.toggleQueueSelection('a');
	for (const kind of ['button', 'checkbox']) {
		const key = event({ target: new Target(kind) });
		run('key', key);
		expect(key.preventDefault).not.toHaveBeenCalled();
	}
	const row = event();
	run('key', row);
	expect(player.snapshot().queueSelection.ids).toEqual(['a', 'b']);
	expect(row.preventDefault).toHaveBeenCalledOnce();
});

function focusFixture(player: ReturnType<typeof createPlayerSession>, action?: 'remove' | 'playNext') {
	class Button extends Target { disabled = false; }
	const button = new Button('button');
	const document = { activeElement: button as unknown, body: {} };
	const focusVisible = vi.fn();
	const focusEmptyTarget = vi.fn();
	const focusFilterEmptyTarget = vi.fn(() => player.snapshot().queue.tracks.length > 0);
	const inputs = new Map(tracks.map(({ id }) => [id, { id }]));
	const list = { querySelectorAll: () => player.snapshot().queue.tracks
		.filter(({ title }) => title.toLowerCase().includes(player.snapshot().queueFilterQuery.toLowerCase()))
		.map(({ id }) => ({ dataset: { trackId: id }, querySelector: () => inputs.get(id) })) };
	let afterFlush = () => {};
	const tick = async () => {
		button.disabled = !player.snapshot().queueSelection.ids.length;
		if (button.disabled || !player.snapshot().queueSelection.active) document.activeElement = document.body;
		afterFlush();
	};
	const compiled = extracted(['reportSelection', 'survivorId', 'focusStranded']);
	const bindings = { player, tick, list, document, HTMLElement: Target, HTMLButtonElement: Button,
		focusVisible, focusEmptyTarget, focusFilterEmptyTarget };
	const run = new Function(...Object.keys(bindings), `
		let selection, queue, visibleEntries, confirmGeneration;
		const selectionControls = player;
		player.subscribe(s => {
			selection = s.queueSelection; queue = s.queue; confirmGeneration = s.queueConfirmGeneration;
			visibleEntries = queue.tracks.filter(t => t.title.toLowerCase().includes(s.queueFilterQuery.toLowerCase())).map(track => ({track}));
		});
		${compiled}
		return reportSelection;
	`)(...Object.values(bindings)) as (event: object, action?: string) => Promise<void>;
	return { document, button, focusVisible, focusEmptyTarget, focusFilterEmptyTarget,
		afterFlush: (callback: () => void) => { afterFlush = callback; }, run: () => run({ currentTarget: button }, action) };
}

it.each([
	[['b', 'c'], '', 'd'], [['d'], '', 'c'], [['a', 'c'], '', 'b']
] as const)('bulk Remove chooses the next surviving displayed selector, then the preceding one: %j', async (ids, query, target) => {
	const { player } = setup();
	ids.forEach((id) => player.toggleQueueSelection(id));
	player.setQueueFilter(query);
	const fixture = focusFixture(player, 'remove');
	await fixture.run();
	expect(fixture.focusVisible).toHaveBeenCalledExactlyOnceWith({ id: target });
	expect(fixture.focusEmptyTarget).not.toHaveBeenCalled();
});

it.each([false, true])('bulk Remove recovers to the honest %s empty target', async (trulyEmpty) => {
	const { player } = setup();
	if (trulyEmpty) player.selectAllQueueRows();
	else { player.toggleQueueSelection('b'); player.setQueueFilter('b'); }
	const fixture = focusFixture(player, 'remove');
	await fixture.run();
	expect(fixture.focusVisible).not.toHaveBeenCalled();
	if (trulyEmpty) expect(fixture.focusEmptyTarget).toHaveBeenCalledOnce();
	else {
		expect(fixture.focusFilterEmptyTarget).toHaveBeenCalledOnce();
		expect(fixture.focusEmptyTarget).not.toHaveBeenCalled();
	}
});

it.each([true, false])('Done returns toolbar focus to a live selector with membership=%s', async (selected) => {
	const { player } = setup();
	player.toggleQueueSelection('c');
	if (!selected) player.toggleQueueSelection('c');
	const fixture = focusFixture(player);
	await fixture.run();
	expect(fixture.focusVisible).toHaveBeenCalledExactlyOnceWith({ id: selected ? 'c' : 'a' });
	expect(player.snapshot().queueSelection.active).toBe(false);
});

it.each(['not held', 'moved', 'auth'] as const)('bulk recovery does not steal focus: %s', async (scenario) => {
	const { player } = setup();
	player.toggleQueueSelection('b');
	const fixture = focusFixture(player, 'remove');
	if (scenario === 'not held') fixture.document.activeElement = {};
	else fixture.afterFlush(() => {
		if (scenario === 'auth') player.resetAtAuthBoundary();
		else fixture.document.activeElement = new Target('button');
	});
	await fixture.run();
	expect(fixture.focusVisible).not.toHaveBeenCalled();
	expect(fixture.focusEmptyTarget).not.toHaveBeenCalled();
	expect(fixture.focusFilterEmptyTarget).not.toHaveBeenCalled();
});

it('a Play next no-op preserves focus and prior feedback', async () => {
	const { player } = setup();
	player.toggleQueueSelection('b');
	player.setQueueFeedback('Previous outcome');
	const fixture = focusFixture(player, 'playNext');
	await fixture.run();
	expect(fixture.document.activeElement).toBe(fixture.button);
	expect(fixture.focusVisible).not.toHaveBeenCalled();
	expect(player.snapshot().queueFeedback).toBe('Previous outcome');
});
