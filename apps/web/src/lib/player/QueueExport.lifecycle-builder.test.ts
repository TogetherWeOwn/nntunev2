import { afterEach, expect, test, vi } from 'vitest';
import ts from 'typescript';
import { createQueue, formatQueueM3U, formatQueueText } from './queue.js';

const sources = import.meta.glob('./QueueExport.svelte', {
	query: '?raw', import: 'default', eager: true
}) as Record<string, string>;
const QUEUE = createQueue([{ id: 'one', title: 'Title', artist: 'Artist', streamUrl: '' }]);

/** Run the real component handlers/effects, replacing only Svelte cells and
 * browser I/O. Protected browser acceptance covers actual effect scheduling,
 * rendered controls, clipboard contents and downloaded bytes. These cases
 * catch leaked Blob URLs and stale settlements across auth/action/teardown.
 */
function component() {
	const source = sources['./QueueExport.svelte'];
	const script = source.slice(source.indexOf('>') + 1, source.indexOf('</script>'));
	const parsed = ts.createSourceFile('export.ts', script, ts.ScriptTarget.Latest, true);
	const body = parsed.statements.filter((node) => !ts.isImportDeclaration(node))
		.map((node) => node.getText(parsed)).join('\n');
	const compiled = ts.transpileModule(body, {
		compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ESNext }
	}).outputText;
	const effects: (() => void)[] = [];
	let destroy = () => {};
	const bindings = {
		formatQueueM3U, formatQueueText,
		$props: () => ({ queue: QUEUE, authGeneration: 0, loading: false }),
		$state: <T>(value: T) => value,
		$effect: (effect: () => void) => effects.push(effect),
		onDestroy: (callback: () => void) => { destroy = callback; }
	};
	const instance = new Function(...Object.keys(bindings), `${compiled}
		return {
			copyQueueText, downloadQueueM3U,
			read: () => ({ status, fallback }),
			changeAccount: () => { authGeneration += 1; }
		};
	`)(...Object.values(bindings)) as {
		copyQueueText: () => Promise<void>;
		downloadQueueM3U: () => void;
		read: () => { status: string; fallback: string | null };
		changeAccount: () => void;
	};
	effects.forEach((effect) => effect());
	return { ...instance, flush: () => effects.forEach((effect) => effect()), destroy: () => destroy() };
}

function browser() {
	const links: { href: string; download: string; hidden: boolean; click: () => void; remove: () => void }[] = [];
	vi.stubGlobal('document', {
		createElement: () => {
			const link = { href: '', download: '', hidden: false, click: vi.fn(), remove: vi.fn() };
			links.push(link);
			return link;
		},
		body: { append: vi.fn() }
	});
	const blobs: Blob[] = [];
	const active = new Set<string>();
	const released: string[] = [];
	const peak: number[] = [];
	vi.spyOn(URL, 'createObjectURL').mockImplementation((value) => {
		blobs.push(value as Blob);
		const url = `blob:queue-${blobs.length}`;
		active.add(url);
		peak.push(active.size);
		return url;
	});
	vi.spyOn(URL, 'revokeObjectURL').mockImplementation((url) => {
		released.push(url);
		active.delete(url);
	});
	return { links, blobs, active, released, peak };
}

afterEach(() => {
	vi.restoreAllMocks();
	vi.unstubAllGlobals();
});

test('repeated downloads retain at most one URL and teardown releases the last one', async () => {
	const io = browser();
	const exportUI = component();
	for (let i = 0; i < 3; i += 1) exportUI.downloadQueueM3U();
	expect(io.peak).toEqual([1, 1, 1]);
	expect(io.released).toEqual(['blob:queue-1', 'blob:queue-2']);
	expect([...io.active]).toEqual(['blob:queue-3']);
	expect(await io.blobs[0].text()).toBe('#EXTM3U\n#EXTINF:-1,Title — Artist\none');
	expect(io.blobs[0].type).toBe('audio/x-mpegurl;charset=utf-8');
	for (const link of io.links) {
		expect(link.download).toBe('queue.m3u');
		expect(link.hidden).toBe(true);
		expect(link.click).toHaveBeenCalledOnce();
		expect(link.remove).toHaveBeenCalledOnce();
	}
	exportUI.destroy();
	expect(io.active.size).toBe(0);
	expect(io.released).toEqual(['blob:queue-1', 'blob:queue-2', 'blob:queue-3']);
});

test('auth reset releases the retained URL and clears export feedback even with the same queue', () => {
	const io = browser();
	const exportUI = component();
	exportUI.downloadQueueM3U();
	expect(exportUI.read().status).toMatch(/download started/i);
	exportUI.changeAccount();
	exportUI.flush();
	expect(io.active.size).toBe(0);
	expect(io.released).toEqual(['blob:queue-1']);
	expect(exportUI.read()).toEqual({ status: '', fallback: null });
});

for (const boundary of ['account before effect', 'teardown', 'new download'] as const) {
	for (const outcome of ['resolve', 'reject'] as const) {
		test(`${boundary} suppresses a pending clipboard ${outcome}`, async () => {
			browser();
			let resolve!: () => void;
			let reject!: (reason: unknown) => void;
			vi.stubGlobal('navigator', { clipboard: { writeText: () => new Promise<void>((yes, no) => {
				resolve = yes; reject = no;
			}) } });
			const exportUI = component();
			const pending = exportUI.copyQueueText();
			if (boundary === 'account before effect') exportUI.changeAccount();
			else if (boundary === 'teardown') exportUI.destroy();
			else exportUI.downloadQueueM3U();
			const before = exportUI.read();
			if (outcome === 'resolve') resolve();
			else reject(new Error('Denied'));
			await pending;
			expect(exportUI.read()).toEqual(before);
		});
	}
}

test('throwing clipboard getter exposes the exact manual-copy text, not a success', async () => {
	const navigator = {};
	Object.defineProperty(navigator, 'clipboard', { get: () => { throw new Error('Denied'); } });
	vi.stubGlobal('navigator', navigator);
	const exportUI = component();
	await exportUI.copyQueueText();
	expect(exportUI.read().fallback).toBe('1. Title — Artist');
	expect(exportUI.read().status).toMatch(/clipboard unavailable/i);
	expect(exportUI.read().status).not.toMatch(/copied/i);
});

test('failed download initiation releases the URL and removes its temporary link', () => {
	const io = browser();
	const exportUI = component();
	vi.mocked(document.body.append).mockImplementation(() => { throw new Error('Blocked'); });
	exportUI.downloadQueueM3U();
	expect(io.active.size).toBe(0);
	expect(io.links[0].remove).toHaveBeenCalledOnce();
	expect(exportUI.read().status).toMatch(/could not start/i);
	expect(exportUI.read().status).not.toMatch(/download started|saved|copied/i);
});
