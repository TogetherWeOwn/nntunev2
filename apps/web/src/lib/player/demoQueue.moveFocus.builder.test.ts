import { describe, expect, it, vi } from 'vitest';
import ts from 'typescript';
import { createQueue, moveToBottom, moveToTop, moveTrack, type QueueState } from './queue.js';
import { compileModule } from 'svelte/compiler';

const sources = import.meta.glob(['../../routes/demo/player/+page.svelte', './test-fixtures/reactiveQueue.svelte.ts'], {
	query: '?raw', import: 'default', eager: true
}) as Record<string, string>;

function moveFunction(): string {
	const source = sources['../../routes/demo/player/+page.svelte']!;
	const start = source.indexOf('>', source.indexOf('<script')) + 1;
	const script = source.slice(start, source.indexOf('</script', start));
	const parsed = ts.createSourceFile('demo.ts', script, ts.ScriptTarget.Latest, true);
	const fn = parsed.statements.find((statement) => ts.isFunctionDeclaration(statement) && statement.name?.text === 'moveQueuedTrack');
	expect(fn).toBeDefined();
	return ts.transpileModule(fn!.getText(parsed), {
		compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ESNext }
	}).outputText;
}

// Vitest's ordinary module transform uses SSR, which erases $state. Compile
// the owned cell for the client and load the runtime named by that output.
const cellScript = ts.transpileModule(sources['./test-fixtures/reactiveQueue.svelte.ts']!, {
	compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ESNext }
}).outputText;
const compiledCell = compileModule(cellScript, { filename: 'reactiveQueue.svelte.js', generate: 'client' }).js.code;
const runtimeImport = compiledCell.match(/import \* as \$ from ['"]([^'"]+)['"];?/);
expect(runtimeImport?.[1]).toBe('svelte/internal/client');
const clientRuntime: unknown = await import(/* @vite-ignore */ runtimeImport![1]);

function reactiveQueue(initial: QueueState): { queue: QueueState } {
	const body = compiledCell.replace(runtimeImport![0], 'const $ = runtime;')
		.replace('export function reactiveQueue', 'function reactiveQueue');
	return new Function('runtime', `${body}\nreturn reactiveQueue;`)(clientRuntime)(initial);
}

function fixture() {
	const original = createQueue(['a', 'b', 'c'].map((id) => ({ id, title: id, artist: '', streamUrl: `/media/${id}` })));
	let release!: () => void;
	const flush = new Promise<void>((resolve) => { release = resolve; });
	const body = {};
	const document: { body: object; activeElement: unknown } = { body, activeElement: null };
	const control = { isConnected: true, disabled: false, focus: vi.fn() };
	document.activeElement = control;
	const cell = reactiveQueue(original);
	const scope = {
		get queue() { return cell.queue; },
		set queue(next: QueueState) { cell.queue = next; },
		queueMoveAnnouncement: undefined
	};
	// Resolve the unmodified handler through real compiled rune assignments,
	// including the proxy identity created when it commits a plain queue object.
	const run = new Function('scope', 'document', 'tick', 'moveToTop', 'moveToBottom', 'moveTrack', `
		with (scope) {
			${moveFunction()}
			return { moveQueuedTrack, snapshot: () => queue, replace: (next) => { queue = next; }, announcement: () => queueMoveAnnouncement };
		}
	`)(scope, document, () => flush, moveToTop, moveToBottom, moveTrack) as {
		moveQueuedTrack: (from: number, to: number, target?: typeof control) => Promise<void>;
		snapshot: () => QueueState;
		replace: (queue: QueueState) => void;
		announcement: () => { queue: QueueState; text: string };
	};
	return { ...run, original: cell.queue, document, control, release };
}

describe('demo queue move action-scoped focus recovery', () => {
	it('repairs only the activated, still-enabled control after the render flush', async () => {
		const f = fixture();
		const done = f.moveQueuedTrack(0, 1, f.control);
		expect(f.snapshot().tracks.map((track) => track.id)).toEqual(['b', 'a', 'c']);
		expect(f.snapshot().tracks[f.snapshot().currentIndex]?.id).toBe('a');
		expect(f.announcement()).toEqual({ queue: f.snapshot(), text: 'a moved to 2 of 3' });
		f.document.activeElement = f.document.body;
		expect(f.control.focus).not.toHaveBeenCalled();
		f.release();
		await done;
		expect(f.control.focus).toHaveBeenCalledExactlyOnceWith({ preventScroll: true });
	});

	it.each(['unfocused', 'retained', 'new-focus', 'detached', 'disabled', 'new-queue'] as const)(
		'does not steal focus for %s', async (reason) => {
			const f = fixture();
			if (reason === 'unfocused') f.document.activeElement = f.document.body;
			const done = f.moveQueuedTrack(0, 1, f.control);
			f.document.activeElement = reason === 'retained' ? f.control : reason === 'new-focus' ? {} : f.document.body;
			if (reason === 'detached') f.control.isConnected = false;
			if (reason === 'disabled') f.control.disabled = true;
			if (reason === 'new-queue') f.replace(createQueue([]));
			f.release();
			await done;
			expect(f.control.focus).not.toHaveBeenCalled();
		}
	);

	it('does not schedule focus for invalid or unchanged moves', async () => {
		const f = fixture();
		await f.moveQueuedTrack(-1, 0, f.control);
		await f.moveQueuedTrack(0, 0, f.control);
		expect(f.snapshot()).toBe(f.original);
		expect(f.control.focus).not.toHaveBeenCalled();
	});

	it('keeps programmatic edits without an activating control focus-neutral', async () => {
		const f = fixture();
		const done = f.moveQueuedTrack(0, 1);
		f.release();
		await done;
		expect(f.snapshot().tracks.map((track) => track.id)).toEqual(['b', 'a', 'c']);
		expect(f.control.focus).not.toHaveBeenCalled();
	});
});
