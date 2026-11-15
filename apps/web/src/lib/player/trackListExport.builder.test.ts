import { afterEach, describe, expect, it, vi } from 'vitest';
import { createTrackListExport } from './trackListExport.js';
import { createQueue, formatQueueM3U, formatQueueText, type QueueState } from './queue.js';

const track = (id: string, title = id) => ({ id, title, artist: 'Artist', streamUrl: '' });
const SAFE = createQueue([track('one', 'First'), track('two', 'Second')]);
const UNSAFE = createQueue([track('one'), track('bad id')]);
const EMPTY = createQueue([]);

type Config = {
	name: string;
	noun: string;
	filename: string;
	unsafeMessage?: string;
	savedDownloads?: boolean;
	keyed: boolean;
	saved: boolean;
};

// Mirrors the four owners' dependency shapes (queue layout, demo, selection, favorites).
const OWNERS: Config[] = [
	{ name: 'layout queue', noun: 'queue', filename: 'queue.m3u', keyed: false, saved: false },
	{ name: 'demo queue', noun: 'queue', filename: 'queue.m3u', savedDownloads: true, keyed: false, saved: true },
	{
		name: 'selection', noun: 'selected tracks', filename: 'selected.m3u', keyed: true, saved: false,
		unsafeMessage: 'Could not export the selection: it contains an unsafe track id.'
	},
	{
		name: 'favorites', noun: 'favorites', filename: 'favorites.m3u', savedDownloads: true, keyed: true, saved: true,
		unsafeMessage: 'Could not export favorites: a favorite has an unsafe track id.'
	}
];

function browser() {
	const events: string[] = [];
	const blobs: Blob[] = [];
	const links: { href: string; download: string; click: () => void; remove: () => void }[] = [];
	vi.stubGlobal('document', {
		createElement: vi.fn(() => {
			const link = { href: '', download: '', hidden: false, click: vi.fn(), remove: vi.fn() };
			links.push(link);
			return link;
		}),
		body: { append: vi.fn() }
	});
	const create = vi.spyOn(URL, 'createObjectURL').mockImplementation((blob) => {
		blobs.push(blob as Blob);
		return `blob:t-${blobs.length}`;
	});
	const revoke = vi.spyOn(URL, 'revokeObjectURL').mockImplementation((url) => { events.push(`revoke:${url}`); });
	return { events, blobs, links, create, revoke };
}

function clipboard() {
	const writes: { resolve: () => void; reject: (e: Error) => void }[] = [];
	const writeText = vi.fn((_: string) => new Promise<void>((resolve, reject) => writes.push({ resolve, reject })));
	vi.stubGlobal('navigator', { clipboard: { writeText } });
	return { writes, writeText };
}

function harness(config: Partial<Config> = {}, initial: QueueState = SAFE) {
	const state = { queue: initial, metadata: 'm1', active: true };
	const outcomes: [string, string | null][] = [];
	let hook: ((status: string) => void) | null = null;
	const exporter = createTrackListExport({
		getQueue: () => state.queue,
		getKey: config.keyed ? () => state.metadata : undefined,
		isActive: () => state.active,
		onOutcome: (status, fallback) => { outcomes.push([status, fallback]); hook?.(status); },
		formatText: formatQueueText,
		formatM3U: formatQueueM3U,
		noun: config.noun ?? 'queue',
		filename: config.filename ?? 'queue.m3u',
		unsafeMessage: config.unsafeMessage,
		savedDownloads: config.savedDownloads
	});
	return { state, outcomes, exporter, onOutcomeHook: (fn: (status: string) => void) => { hook = fn; } };
}

afterEach(() => {
	vi.restoreAllMocks();
	vi.unstubAllGlobals();
});

describe.each(OWNERS)('$name owner messages', (config) => {
	it('copies real formatted text and reports the exact copied message', async () => {
		const { writes, writeText } = clipboard();
		const { exporter, outcomes } = harness(config);
		const pending = exporter.copy();
		writes[0].resolve();
		await pending;
		expect(writeText).toHaveBeenCalledExactlyOnceWith('1. First — Artist\n2. Second — Artist');
		expect(outcomes.at(-1)).toEqual([`Copied ${config.noun} as text.`, null]);
	});

	it('offers the exact manual fallback when the write is refused', async () => {
		const { writes } = clipboard();
		const { exporter, outcomes } = harness(config);
		const pending = exporter.copy();
		writes[0].reject(new Error('Denied'));
		await pending;
		expect(outcomes.at(-1)).toEqual([
			`Clipboard unavailable. Copy the ${config.noun} manually below.`,
			'1. First — Artist\n2. Second — Artist'
		]);
	});

	it('downloads real M3U bytes with the owner filename and exact initiation message', async () => {
		const env = browser();
		const { exporter, outcomes } = harness(config);
		exporter.download();
		expect(await env.blobs[0].text()).toBe(formatQueueM3U(SAFE));
		expect(env.links[0].download).toBe(config.filename);
		const where = config.saved ? 'saved downloads' : 'downloads';
		expect(outcomes.at(-1)).toEqual([`Download started. Check your browser’s ${where} for ${config.filename}.`, null]);
		exporter.release();
	});

	it('reports the exact unsafe (or generic download) message for unsafe ids', async () => {
		const env = browser();
		const { writes, writeText } = clipboard();
		const { exporter, outcomes } = harness(config, UNSAFE);
		const pending = exporter.copy();
		writes[0]?.resolve();
		await pending;
		const copyOutcome = outcomes.at(-1);
		exporter.download();
		const downloadOutcome = outcomes.at(-1);
		expect(env.create).not.toHaveBeenCalled();
		if (config.unsafeMessage) {
			expect(writeText).not.toHaveBeenCalled();
			expect(copyOutcome).toEqual([config.unsafeMessage, null]);
			expect(downloadOutcome).toEqual([config.unsafeMessage, null]);
		} else {
			// Text export has no id safety requirement; only the M3U path refuses.
			expect(writes).toHaveLength(1);
			expect(downloadOutcome).toEqual([`Could not start the download. Try again or copy the ${config.noun} as text.`, null]);
		}
	});

	it('publishes the exact download failure and releases the URL when the browser refuses', () => {
		const env = browser();
		const { exporter, outcomes } = harness(config);
		exporter.download();
		env.links[0].click = vi.fn();
		vi.mocked(document.createElement).mockImplementationOnce(() => { throw new Error('No DOM'); });
		exporter.download();
		expect(outcomes.at(-1)).toEqual([`Could not start the download. Try again or copy the ${config.noun} as text.`, null]);
		expect(env.revoke).toHaveBeenCalledExactlyOnceWith('blob:t-1');
	});
});

describe('inactive and empty owners', () => {
	it('does nothing visible beyond clearing feedback when inactive', async () => {
		const env = browser();
		const { writeText } = clipboard();
		const { exporter, state, outcomes } = harness();
		state.active = false;
		await exporter.copy();
		exporter.download();
		expect(writeText).not.toHaveBeenCalled();
		expect(env.create).not.toHaveBeenCalled();
		expect(outcomes.every(([status, fallback]) => status === '' && fallback === null)).toBe(true);
	});

	it('does not touch clipboard, URL or document for an empty queue', async () => {
		const env = browser();
		const { writeText } = clipboard();
		const { exporter } = harness({}, EMPTY);
		await exporter.copy();
		exporter.download();
		expect(writeText).not.toHaveBeenCalled();
		expect(env.create).not.toHaveBeenCalled();
		expect(document.createElement).not.toHaveBeenCalled();
	});
});

describe('download URL lifetime', () => {
	it('owners with and without saved-downloads wording both release the previous URL before the next payload', () => {
		for (const savedDownloads of [false, true]) {
			const env = browser();
			const { exporter } = harness({ savedDownloads });
			exporter.download();
			exporter.download();
			expect(env.revoke).toHaveBeenCalledExactlyOnceWith('blob:t-1');
			exporter.release();
			expect(env.revoke.mock.calls).toEqual([['blob:t-1'], ['blob:t-2']]);
			vi.restoreAllMocks();
		}
	});

	it('retains the URL across copies and reset(false), replaces on re-download, frees on reset(true)', async () => {
		const env = browser();
		const { writes } = clipboard();
		const { exporter } = harness();
		exporter.download();
		const pending = exporter.copy();
		writes[0].resolve();
		await pending;
		exporter.reset();
		exporter.reset(false);
		expect(env.revoke).not.toHaveBeenCalled();
		exporter.download();
		expect(env.revoke.mock.calls).toEqual([['blob:t-1']]);
		exporter.reset(true);
		expect(env.revoke.mock.calls).toEqual([['blob:t-1'], ['blob:t-2']]);
		exporter.reset(true);
		exporter.release();
		expect(env.revoke).toHaveBeenCalledTimes(2);
	});
});

describe('fencing of pending clipboard writes', () => {
	const settle = async (rejected: boolean, writes: { resolve: () => void; reject: (e: Error) => void }[], p: Promise<void>) => {
		if (rejected) writes[0].reject(new Error('Denied'));
		else writes[0].resolve();
		await p;
	};

	for (const rejected of [false, true]) {
		const label = rejected ? 'rejection' : 'success';

		it(`metadata-keyed owner retains a same-key snapshot refresh across ${label}`, async () => {
			const { writes } = clipboard();
			const { exporter, state, outcomes } = harness({ keyed: true });
			const pending = exporter.copy();
			state.queue = createQueue([track('one', 'First'), track('two', 'Second')]);
			await settle(rejected, writes, pending);
			expect(outcomes.at(-1)![0]).toBe(rejected ? 'Clipboard unavailable. Copy the queue manually below.' : 'Copied queue as text.');
		});

		for (const [what, change] of [
			['metadata', (s: { queue: QueueState; metadata: string }) => { s.metadata = 'm2'; }],
			['membership', (s: { queue: QueueState; metadata: string }) => { s.queue = createQueue([track('one')]); s.metadata = 'm-members'; }],
			['order', (s: { queue: QueueState; metadata: string }) => { s.queue = createQueue([track('two'), track('one')]); s.metadata = 'm-order'; }]
		] as const) {
			it(`metadata-keyed owner cancels on changed ${what} across ${label}`, async () => {
				const { writes } = clipboard();
				const { exporter, state, outcomes } = harness({ keyed: true });
				const pending = exporter.copy();
				change(state);
				await settle(rejected, writes, pending);
				expect(outcomes).toEqual([['', null]]);
			});
		}

		it(`identity-keyed owner cancels on any new snapshot object across ${label}`, async () => {
			const { writes } = clipboard();
			const { exporter, state, outcomes } = harness();
			const pending = exporter.copy();
			state.queue = { ...SAFE };
			await settle(rejected, writes, pending);
			expect(outcomes).toEqual([['', null]]);
		});

		it(`a newer copy supersedes the older one across ${label}`, async () => {
			const { writes } = clipboard();
			const { exporter, outcomes } = harness();
			const first = exporter.copy();
			const second = exporter.copy();
			await Promise.resolve();
			await settle(rejected, [writes[0]], first);
			expect(outcomes.filter(([s]) => s !== '')).toEqual([]);
			await settle(rejected, [writes[1]], second);
			expect(outcomes.filter(([s]) => s !== '')).toHaveLength(1);
		});

		it(`reset, release-reset and disposal supersede a pending write across ${label}`, async () => {
			for (const how of ['reset', 'reset-release', 'inactive'] as const) {
				browser();
				const { writes } = clipboard();
				const { exporter, state, outcomes } = harness();
				const pending = exporter.copy();
				if (how === 'reset') exporter.reset();
				else if (how === 'reset-release') exporter.reset(true);
				else state.active = false;
				await settle(rejected, writes, pending);
				expect(outcomes.filter(([s, f]) => s !== '' || f !== null)).toEqual([]);
			}
		});
	}

	it('does not mutate the queue snapshot', async () => {
		browser();
		const { writes } = clipboard();
		const queue = createQueue([track('one', 'First'), track('two', 'Second')]);
		const before = JSON.stringify(queue);
		const { exporter, state } = harness({}, queue);
		state.queue = queue;
		const pending = exporter.copy();
		exporter.download();
		writes[0]?.resolve();
		await pending;
		expect(JSON.stringify(queue)).toBe(before);
	});

	it('a key change during selection safety validation prevents the download', () => {
		const env = browser();
		const state = { key: 1 };
		const outcomes: string[] = [];
		const exporter = createTrackListExport({
			getQueue: () => SAFE,
			getKey: () => state.key,
			isActive: () => true,
			onOutcome: (status) => outcomes.push(status),
			formatText: formatQueueText,
			formatM3U: (q) => { state.key++; return formatQueueM3U(q); },
			noun: 'selected tracks',
			filename: 'selected.m3u',
			unsafeMessage: 'Could not export the selection: it contains an unsafe track id.'
		});
		exporter.download();
		expect(env.create).not.toHaveBeenCalled();
		expect(outcomes).toEqual(['']);
	});
});

describe('clipboard access', () => {
	it('treats a throwing clipboard getter as unavailable and exposes the fallback text', async () => {
		vi.stubGlobal('navigator', { get clipboard(): never { throw new Error('Blocked'); } });
		const { exporter, outcomes } = harness();
		await exporter.copy();
		expect(outcomes.at(-1)).toEqual(['Clipboard unavailable. Copy the queue manually below.', '1. First — Artist\n2. Second — Artist']);
	});

	it.each([undefined, null, {}, { writeText: 'nope' }])('treats clipboard %j as unavailable', async (value) => {
		vi.stubGlobal('navigator', { clipboard: value });
		const { exporter, outcomes } = harness();
		await exporter.copy();
		expect(outcomes.at(-1)![0]).toBe('Clipboard unavailable. Copy the queue manually below.');
	});

	it('treats a synchronously throwing writeText as unavailable', async () => {
		vi.stubGlobal('navigator', { clipboard: { writeText: () => { throw new Error('Sync'); } } });
		const { exporter, outcomes } = harness();
		await exporter.copy();
		expect(outcomes.at(-1)![0]).toContain('Clipboard unavailable');
	});

	it('invokes writeText with the clipboard as receiver and reads the getter once', async () => {
		let reads = 0;
		const receivers: unknown[] = [];
		const clip = {
			writeText(this: unknown) { receivers.push(this); return Promise.resolve(); }
		};
		vi.stubGlobal('navigator', { get clipboard() { reads++; return clip; } });
		const { exporter, outcomes } = harness();
		await exporter.copy();
		expect(receivers).toEqual([clip]);
		expect(reads).toBe(1);
		expect(outcomes.at(-1)).toEqual(['Copied queue as text.', null]);
	});
});

describe('subscriber-triggered nested reset during onOutcome', () => {
	it('stops a copy superseded by a reset inside its own initial clear', async () => {
		const { writeText } = clipboard();
		const { exporter, onOutcomeHook } = harness();
		let fired = false;
		onOutcomeHook(() => { if (!fired) { fired = true; exporter.reset(); } });
		await exporter.copy();
		expect(writeText).not.toHaveBeenCalled();
	});

	it('stops a download superseded by a nested reset inside its initial clear', () => {
		const env = browser();
		const { exporter, onOutcomeHook, outcomes } = harness();
		let fired = false;
		onOutcomeHook(() => { if (!fired) { fired = true; exporter.reset(); } });
		exporter.download();
		expect(env.create).not.toHaveBeenCalled();
		expect(outcomes.every(([s]) => s === '')).toBe(true);
	});

	it('a nested reset(true) after a download outcome frees the new URL', () => {
		const env = browser();
		const { exporter, onOutcomeHook } = harness();
		onOutcomeHook((status) => { if (status.startsWith('Download started')) exporter.reset(true); });
		exporter.download();
		expect(env.revoke).toHaveBeenCalledExactlyOnceWith('blob:t-1');
	});
});
