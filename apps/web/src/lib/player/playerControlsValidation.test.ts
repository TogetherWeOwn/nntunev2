import ts from 'typescript';
import { render } from 'svelte/server';
import { expect, test, vi } from 'vitest';
import { formatDuration } from '../format.js';
import { createM3UDownload, m3uDownloadFailure } from './m3uDownload.js';
import { createQueue, DEMO_TRACKS, formatQueueM3U, formatQueueText } from './queue.js';
import QueueExport from './QueueExport.svelte';

const sources = import.meta.glob('./*.svelte', { query: '?raw', import: 'default' });

async function handlers<T>(
	name: string, props: object, result: string, ports: Record<string, unknown> = {}
): Promise<T> {
	const source = await sources[`./${name}.svelte`]() as string;
	const script = source.slice(source.indexOf('>') + 1, source.indexOf('</script>'));
	const body = script.replace(/^\s*import .*;$/gm, '');
	const compiled = ts.transpileModule(body, { compilerOptions: { target: ts.ScriptTarget.ES2022 } }).outputText;
	const cell = <T>(value: T) => value;
	const bindings = {
		$props: Object.assign(() => props, { id: () => 'id' }),
		$state: cell, $derived: cell, $effect: () => {},
		untrack: (read: () => unknown) => read(), formatDuration,
		...ports
	};
	return new Function(...Object.keys(bindings), `${compiled}\nreturn ${result};`)(...Object.values(bindings));
}

test('queue export refuses new actions and pending clipboard feedback during loading', async () => {
	const props = { queue: createQueue(DEMO_TRACKS), authGeneration: 0 };
	for (const loading of [true, false]) {
		const { body } = render(QueueExport, { props: { ...props, loading } });
		expect(body.match(/<button[^>]*\sdisabled\b/g) ?? []).toHaveLength(loading ? 2 : 0);
	}
	const { promise, reject } = Promise.withResolvers<void>();
	const writeText = vi.fn(() => promise);
	const document = { createElement: vi.fn() };
	const ui = await handlers<{
		copyQueueText(): Promise<void>; downloadQueueM3U(): void; startLoading(): void; read(): string;
	}>('QueueExport', props,
		'({copyQueueText, downloadQueueM3U, startLoading: () => { loading = true; }, read: () => status})',
		{ createM3UDownload, m3uDownloadFailure, formatQueueM3U, formatQueueText, onDestroy: () => {},
			navigator: { clipboard: { writeText } }, document });
	const pending = ui.copyQueueText();
	ui.startLoading();
	await ui.copyQueueText();
	ui.downloadQueueM3U();
	reject(new Error('Clipboard denied'));
	await pending;
	expect(writeText).toHaveBeenCalledOnce();
	expect(document.createElement).not.toHaveBeenCalled();
	expect(ui.read()).toBe('');
});

test('sleep timer refuses invalid durations and unsafe track counts, while accepting fractional minutes', async () => {
	const start = vi.fn();
	const submit = await handlers<(field: string, value: string) => void>('SleepTimerControls', { start }, 'submit');
	submit('minutes', '1e100');
	submit('tracks', '9007199254740992');
	expect(start).not.toHaveBeenCalled();
	submit('minutes', '0.5');
	expect(start).toHaveBeenCalledWith({ kind: 'duration', ms: 30_000 }, 'Sleep in 0.5 minutes');
});

test('volume refuses disabled and invalid requests and restores a valid observed thumb', async () => {
	const onvolumeinput = vi.fn(() => NaN);
	const control = { valueAsNumber: 50 };
	const props = { volume: 0.75, ready: true, onvolumeinput };
	const handler = (restricted: boolean) => handlers<(event: object) => void>(
		'VolumeControls', { ...props, restricted }, 'handleInput');
	(await handler(true))({ currentTarget: control });
	expect(control.valueAsNumber).toBe(75);
	const change = await handler(false);
	for (const requested of [101, 50]) {
		control.valueAsNumber = requested;
		change({ currentTarget: control });
	}
	expect(onvolumeinput.mock.calls).toEqual([[0.5]]);
	expect(control.valueAsNumber).toBe(75);
});

test('speed dispatches only preset requests and restores non-preset readback', async () => {
	const onchange = vi.fn();
	const control = { value: 'Infinity' };
	const change = await handlers<(control: { value: string }) => void>(
		'PlaybackSpeedControl', { value: '1.3', onchange }, 'change');
	change(control);
	expect(control.value).toBe('1.3');
	control.value = '1.5';
	change(control);
	expect(onchange.mock.calls).toEqual([[control]]);
});

test('seek clamps keyboard nudges, rejects invalid input and keeps pointer replay tied to its gesture', async () => {
	const seek = vi.fn();
	const state = { attachment: 7, enabled: true, duration: 20, position: 0.2, elapsed: 0.2 };
	const ui = await handlers<Record<'input' | 'keydown' | 'beginPointer' | 'endPointer', (event: object) => void>>(
		'PlayerSeek', { state, seek }, '({input, keydown, beginPointer, endPointer})');
	const control = { valueAsNumber: Infinity };
	const preventDefault = vi.fn();
	ui.keydown({ key: 'End', preventDefault, defaultPrevented: true });
	ui.input({ currentTarget: control });
	expect(seek).not.toHaveBeenCalled();
	expect(control.valueAsNumber).toBe(0.2);
	ui.keydown({ key: 'ArrowLeft', preventDefault });
	ui.beginPointer({ pointerId: 1, isPrimary: true, button: 0 });
	for (const value of [5, 6]) {
		control.valueAsNumber = value;
		ui.input({ currentTarget: control });
		ui.endPointer({ pointerId: 2 });
	}
	expect(seek.mock.calls).toEqual([[7, 0, true], [7, 5, true], [7, 6, false]]);
});
