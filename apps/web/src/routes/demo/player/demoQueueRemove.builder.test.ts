import { afterEach, expect, test, vi } from 'vitest';
import ts from 'typescript';
import * as queueFunctions from '#lib/player/queue.js';
import { matchesQueueQuery } from '#lib/player/queueFilter.js';
import { createPlayback, endedForCurrent } from '#lib/player/playback.js';
import { createRadioOrder } from '#lib/player/radio.js';
import { createSleepTimer } from '#lib/player/sleepTimer.js';
import { createResumeStore } from '#lib/player/resume.js';
import { deriveTrackTime } from '#lib/format.js';
import { render } from 'svelte/server';
import { compileModule } from 'svelte/compiler';
// @ts-expect-error Svelte's compiler runtime has no published TypeScript declarations.
import * as runes from 'svelte/internal/client';
import DemoPlayer from './+page.svelte';

vi.mock('$app/state', () => ({ page: { url: new URL('http://localhost/demo/player') } }));

const sources = import.meta.glob('./+page.svelte', { query: '?raw', import: 'default', eager: true }) as Record<string, string>;

// Execute real handlers and Svelte's reactive state/derived expressions with a
// controlled DOM flush. Mount/effects and live-region commits belong to the
// hydrated browser companion, not this handler harness.
function harness() {
	const source = sources['./+page.svelte'];
	const open = source.indexOf('<script');
	const script = source.slice(source.indexOf('>', open) + 1, source.indexOf('</script', open));
	const parsed = ts.createSourceFile('demo.ts', script, ts.ScriptTarget.Latest, true);
	const body = parsed.statements.filter((node) => {
		if (ts.isImportDeclaration(node)) return false;
		return !(ts.isExpressionStatement(node) && ts.isCallExpression(node.expression) &&
			node.expression.expression.getText(parsed) === '$effect');
	}).map((node) => node.getText(parsed)).join('\n');
	const pending: (() => void)[] = [];
	const storage = new Map<string, string>();
	const resume = createResumeStore({ storage: {
		getItem: (key) => storage.get(key) ?? null,
		setItem: (key, value) => { storage.set(key, value); },
		removeItem: (key) => { storage.delete(key); }
	} });
	const bodyElement = {};
	const dom = { body: bodyElement, activeElement: bodyElement as unknown, hasFocus: () => true };
	vi.stubGlobal('document', dom);
	const heading = { focus: vi.fn(() => { dom.activeElement = heading; }) };
	const input = { focus: vi.fn(() => { dom.activeElement = input; }) };
	const list = { isConnected: true, querySelectorAll: () => buttons };
	const buttons = queueFunctions.DEMO_TRACKS.map((track) => ({
		id: track.id, isConnected: true,
		focus: vi.fn(function (this: unknown) { dom.activeElement = this; }),
		closest: () => list
	}));
	// Real DOM nodes are not plain objects: Svelte must retain this node's identity.
	const audio = Object.assign(new EventTarget(), { currentTime: 45, ended: false, play: vi.fn(() => Promise.resolve()), pause: vi.fn(), load: vi.fn(), removeAttribute: vi.fn(), getAttribute: () => '/api/demo/audio' });
	const bindings = {
		...queueFunctions, matchesQueueQuery, createPlayback, endedForCurrent, createRadioOrder, createSleepTimer, deriveTrackTime,
		tick: () => new Promise<void>((resolve) => pending.push(resolve)),
		onMount: () => {}
	};
	const activation = source.match(/data-testid="queue-remove-\{track.id\}"[\s\S]*?onclick=\{\(event\) => ([^}]+)\}/)?.[1];
	if (!activation) throw new Error('Remove activation missing');
	const scriptWithAccessors = ts.transpileModule(`export function createHarnessPage() {
		${body}
		return {
			activate: (track, index, event) => ${activation},
			addToQueue, replayRecent, enqueueRecent, persistPosition, releaseAudio, playCurrent, startRadioPass, selectAt, playNextAt,
			setup: (store, audio, heading, input) => { resumeStore = store; audioEl = audio; queueEmptyFocus = heading; demoFilterInput = input; },
			setFilter: (value) => { demoFilterQuery = value; },
			read: () => ({ queue, radio, announcement: queueAnnouncement, status })
		};
	}`, { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ESNext } }).outputText;
	const compiled = ts.transpileModule(compileModule(scriptWithAccessors, {
		filename: 'demo-removal-harness.svelte.js', generate: 'client', dev: false
	}).js.code, { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS } }).outputText;
	const exports = {} as { createHarnessPage: () => any };
	new Function('require', 'exports', ...Object.keys(bindings), compiled)(
		(name: string) => {
			if (name !== 'svelte/internal/client') throw new Error(`Unexpected harness import: ${name}`);
			return runes;
		}, exports, ...Object.values(bindings)
	);
	const page = exports.createHarnessPage();
	page.setup(resume, audio, heading, input);
	return { page, dom, list, buttons, heading, input, audio, resume,
		flush: async () => { for (const resolve of pending.splice(0)) resolve(); await Promise.resolve(); },
		// The event is created from the same row identity used by the template.
		remove: (id: string) => {
			const button = buttons.find((button) => button.id === id)!;
			const index = queueFunctions.DEMO_TRACKS.findIndex((track) => track.id === id);
			return page.activate(queueFunctions.DEMO_TRACKS[index], index, { currentTarget: button });
		}
	};
}

afterEach(() => vi.unstubAllGlobals());

test('initial queue counts stay readable without pre-filling the shared announcement', () => {
	const { body } = render(DemoPlayer);
	const region = body.match(/<p data-testid="queue-announcement"[^>]*>([\s\S]*?)<\/p>/)?.[1];
	expect(region).toBeDefined();
	// Accept only whitespace and inert SSR markers, not stripped HTML.
	expect(region).toMatch(/^(?:\s|<!--[^>]*-->)*$/);
	expect(body).toMatch(/<p role="status" aria-live="off">3 of 3 tracks<\/p>/);
});

test('the rendered queue has one emitting status region for filtering and removals', () => {
	const { body } = render(DemoPlayer);
	const panel = body.slice(body.indexOf('<section data-testid="queue-panel"'), body.indexOf('</section>', body.indexOf('<section data-testid="queue-panel"')));
	const regions = (panel.match(/<p\b[^>]*>[\s\S]*?<\/p>/g) ?? []).filter((region) =>
		region.includes('data-testid="queue-announcement"') || /\d+ of \d+ tracks/.test(region));
	expect(regions.filter((region) => !region.slice(0, region.indexOf('>')).includes('aria-live="off"'))).toHaveLength(1);
});

test('identity-based removal tolerates two activations before the DOM flush', async () => {
	const h = harness();
	await h.remove('demo-track');
	await h.remove('second-signal');
	expect(h.page.read().queue.tracks.map((track: { id: string }) => track.id)).toEqual(['third-light']);
	expect(h.page.read().announcement).toBe('Second Signal removed from the queue');
	expect(h.page.read().status).toBe('selected');
});

test('a repeated activation of an already removed identity is a no-op', async () => {
	const h = harness();
	await h.remove('demo-track');
	await h.remove('demo-track');
	expect(h.page.read().queue.tracks.map((track: { id: string }) => track.id)).toEqual(['second-signal', 'third-light']);
});

test('current removal clears resume and fences the outgoing audio teardown, not future audio', async () => {
	const h = harness();
	h.resume.save('demo-track', 25);
	h.resume.save('second-signal', 12);
	const outgoing = h.page.releaseAudio(h.audio, 'demo-track');
	await h.remove('demo-track');
	outgoing.destroy();
	expect(h.resume.positionFor('demo-track')).toBeNull();
	expect(h.resume.positionFor('second-signal')).toBe(12);
	const freshAudio = { ...h.audio, currentTime: 18 };
	h.page.persistPosition('demo-track', freshAudio);
	expect(h.resume.positionFor('demo-track')).toBe(18);
});

// Removing playback.pause() from removeAt must let this pending play escape.
test('current removal cancels a play still waiting for the DOM flush', async () => {
	const h = harness();
	h.page.playCurrent();
	expect(h.page.read().status).toBe('loading');
	expect(h.audio.play).not.toHaveBeenCalled();
	await h.remove('demo-track');
	await h.flush();
	expect(h.page.read().queue.tracks[h.page.read().queue.currentIndex].id).toBe('second-signal');
	expect(h.page.read().status).toBe('selected');
	expect(h.audio.play).not.toHaveBeenCalled();
});

// Dropping !radio from removeAt must pause this active radio pass.
test('removing the queue current slot preserves active radio playback', async () => {
	const h = harness();
	h.page.startRadioPass(7);
	await h.flush();
	expect(h.page.read().status).toBe('playing');
	expect(h.audio.play).toHaveBeenCalledOnce();
	const radio = h.page.read().radio;
	expect(radio).not.toBeNull();
	const pauses = h.audio.pause.mock.calls.length;
	await h.remove('demo-track');
	await h.flush();
	expect(h.page.read().queue.tracks.map((track: { id: string }) => track.id)).toEqual(['second-signal', 'third-light']);
	expect(h.page.read().radio).toBe(radio);
	expect(h.page.read().status).toBe('playing');
	expect(h.audio.pause).toHaveBeenCalledTimes(pauses);
	expect(h.audio.play).toHaveBeenCalledOnce();
});

// The pending tail pick must not fence the original source if removal returns to it.
test('same-turn tail selection and removal keeps the surviving audio resumable', async () => {
	const h = harness();
	await h.remove('second-signal');
	h.audio.currentTime = 18;
	h.page.selectAt(1);
	await h.remove('third-light');
	await h.flush();
	expect(h.page.read().queue.tracks.map((track: { id: string }) => track.id)).toEqual(['demo-track']);
	expect(h.page.read().status).toBe('selected');
	expect(h.audio.play).not.toHaveBeenCalled();
	h.audio.currentTime = 45;
	h.page.persistPosition('demo-track', h.audio);
	expect(h.resume.positionFor('demo-track')).toBe(45);
});

// Active radio owns the source even when removal empties the resting queue.
for (const repeat of ['off', 'one', 'queue'] as const) {
	test(`radio advances after the last queued row is removed with repeat ${repeat}`, () => {
		const empty = queueFunctions.createQueue([]);
		expect(queueFunctions.naturalEndPolicy(empty, {
			repeat, radioActive: true, continueRadio: false,
			playing: true, validEnd: true, sleepComplete: false
		})).toEqual({ queue: empty, action: 'radio' });
	});
}

for (const [patch, action] of [
	[{ sleepComplete: true }, 'stop'],
	[{ playing: false }, 'ignore'],
	[{ validEnd: false }, 'ignore'],
	[{ radioActive: false, continueRadio: true }, 'ignore'],
	[{ radioActive: false, sleepComplete: true }, 'ignore']
] as const) {
	test(`empty resting queue honors natural-end boundary ${JSON.stringify(patch)}`, () => {
		const empty = queueFunctions.createQueue([]);
		expect(queueFunctions.naturalEndPolicy(empty, {
			repeat: 'off', radioActive: true, continueRadio: false,
			playing: true, validEnd: true, sleepComplete: false, ...patch
		})).toEqual({ queue: empty, action });
	});
}

test('a no-op history enqueue preserves the removal confirmation', async () => {
	const h = harness();
	await h.remove('second-signal');
	h.page.enqueueRecent('demo-track');
	expect(h.page.read().announcement).toBe('Second Signal removed from the queue');
	expect(h.audio.play).not.toHaveBeenCalled();
});

test('noncurrent removal leaves the playing element and its resume behavior alone', async () => {
	const h = harness();
	await h.remove('second-signal');
	expect(h.audio.pause).not.toHaveBeenCalled();
	h.page.persistPosition('demo-track', h.audio);
	expect(h.resume.positionFor('demo-track')).toBe(45);
});

test('the last visible removal focuses the filter while hidden tracks remain', async () => {
	const h = harness();
	h.page.setFilter('second');
	h.dom.activeElement = h.buttons[1];
	const flight = h.remove('second-signal');
	h.buttons[1].isConnected = false;
	// A filtered DOM contains just the removed button.
	for (const button of [h.buttons[0], h.buttons[2]]) button.isConnected = false;
	h.dom.activeElement = h.dom.body;
	await h.flush();
	await flight;
	expect(h.input.focus).toHaveBeenCalledOnce();
	expect(h.heading.focus).not.toHaveBeenCalled();
});

for (const boundary of ['null', 'blur', 'other-control'] as const) {
	test(`focus recovery refuses a newer ${boundary} boundary`, async () => {
		const h = harness();
		h.dom.activeElement = h.buttons[1];
		const flight = h.remove('second-signal');
		h.buttons[1].isConnected = false;
		h.dom.activeElement = boundary === 'null' ? null : boundary === 'other-control' ? h.input : h.dom.body;
		if (boundary === 'blur') h.dom.hasFocus = () => false;
		await h.flush();
		await flight;
		for (const target of [...h.buttons, h.heading, h.input]) expect(target.focus).not.toHaveBeenCalled();
	});
}

test('an unmounted panel cancels pending focus recovery before its bindings disappear', async () => {
	const h = harness();
	h.dom.activeElement = h.buttons[1];
	const result = h.remove('second-signal').then(() => null, (error: unknown) => error);
	h.list.isConnected = false;
	for (const button of h.buttons) button.isConnected = false;
	h.page.setup(h.resume, h.audio, undefined, undefined);
	h.dom.activeElement = h.dom.body;
	await h.flush();
	expect(await result).toBeNull();
	for (const target of [h.heading, h.input]) expect(target.focus).not.toHaveBeenCalled();
});

for (const via of ['add', 'recent', 'enqueue-recent'] as const) {
	test(`only re-adding the last removed track via ${via} clears its stale confirmation`, async () => {
		const h = harness();
		await h.remove('second-signal');
		await h.remove('third-light');
		if (via === 'add') h.page.addToQueue(queueFunctions.DEMO_TRACKS[1]);
		else if (via === 'recent') h.page.replayRecent('second-signal');
		else h.page.enqueueRecent('second-signal');
		expect(h.page.read().announcement).toBe('Third Light removed from the queue');
		if (via === 'add') h.page.addToQueue(queueFunctions.DEMO_TRACKS[2]);
		else if (via === 'recent') h.page.replayRecent('third-light');
		else h.page.enqueueRecent('third-light');
		expect(h.page.read().announcement).toBe('');
	});
}

for (const via of ['add', 'recent', 'enqueue-recent'] as const) {
	test(`re-adding via ${via} invalidates the positional snapshot without resurrecting removal`, async () => {
		const h = harness();
		await h.remove('demo-track');
		h.page.selectAt(1);
		const move = h.page.playNextAt(0, h.buttons[1]);
		await h.flush();
		await move;
		expect(h.page.read().announcement).toBe('Second Signal moved to position 2 of 2');
		expect(h.page.read().queue.tracks.map((track: { id: string }) => track.id)).toEqual(['third-light', 'second-signal']);
		if (via === 'add') h.page.addToQueue(queueFunctions.DEMO_TRACKS[0]);
		else if (via === 'recent') h.page.replayRecent('demo-track');
		else h.page.enqueueRecent('demo-track');
		expect(h.page.read().queue.tracks.map((track: { id: string }) => track.id)).toEqual(['third-light', 'second-signal', 'demo-track']);
		expect(h.page.read().announcement).toBe('');
	});
}

test('a no-op add does not erase a removal confirmation', async () => {
	const h = harness();
	await h.remove('second-signal');
	h.page.addToQueue(queueFunctions.DEMO_TRACKS[0]);
	expect(h.page.read().announcement).toBe('Second Signal removed from the queue');
});
