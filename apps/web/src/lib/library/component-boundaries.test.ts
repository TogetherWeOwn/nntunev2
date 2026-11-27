import ts from 'typescript';
import { expect, test, vi } from 'vitest';
import * as ingest from '../admin-ingest/requests.js';
import { ApiError } from '../api/client.js';

const sources = import.meta.glob(['./*.svelte', '../admin-ingest/*.svelte'], { query: '?raw', import: 'default', eager: true });
const identity = <T>(value: T) => value;
const owner = { run: async <T>(work: () => T) => work(), isCurrent: () => true };
const observe = () => ({ capture: async () => owner, prepare: vi.fn(), dispose: vi.fn() });

// Run the real handlers with injected I/O. Tests drive effect callbacks and derived values explicitly.
function component(path: string, overrides: Record<string, unknown>, expose: string) {
	const script = (sources[path] as string).match(/<script[^>]*>([\s\S]*?)<\/script>/)![1];
	const parsed = ts.createSourceFile('component.ts', script, ts.ScriptTarget.Latest, true);
	const body = parsed.statements.filter((node) => !ts.isImportDeclaration(node)).map((node) => node.getText(parsed)).join('\n');
	const { outputText } = ts.transpileModule(body.replace('const selectionKey =', 'let selectionKey ='), {
		compilerOptions: { target: ts.ScriptTarget.ES2022 }
	});
	const bindings = {
		$props: () => ({}), $state: Object.assign(identity, { raw: identity }), $derived: identity,
		$effect: vi.fn(), onMount: vi.fn(), onDestroy: vi.fn(), untrack: (work: () => unknown) => work(),
		ApiClient: class {}, ApiError, PUBLIC_API_BASE_URL: '', observeAccountSession: observe, ...overrides
	};
	return new Function(...Object.keys(bindings), `${outputText}\nreturn ${expose};`)(...Object.values(bindings));
}

test.each([0, 401, 403, 412])('liked shelf feedback after a failed read (%i) stays within its scope', async (status) => {
	const effect = vi.fn();
	const observeSession = vi.fn((_clear: () => void) => observe());
	const items = [{ kind: 'track', targetId: 'old-song' }];
	const loadLikes = vi.fn().mockResolvedValue({ binding: 'old', items });
	const view = component('./AccountLikes.svelte', {
		$props: () => ({ libraryBinding: 'old' }), $effect: effect, observeAccountSession: observeSession,
		loadLikes, verifyLibrarySession: async () => true
	}, "{ read, report: (message) => { feedback = message; }, switchBinding: () => { libraryBinding = 'new'; }, state: () => ({ likes, phase, feedback }) }");
	await view.read(new AbortController().signal, owner, 'old');
	expect(view.state()).toEqual({ likes: items, phase: 'ready', feedback: '' });
	view.report('Imported.');
	loadLikes.mockRejectedValueOnce(status ? new ApiError(status, 'denied', 'Denied') : new Error('Offline'));
	await view.read(new AbortController().signal, owner, 'old');
	expect(view.state()).toEqual({ likes: [], phase: status === 401 ? 'sign-in' : 'error', feedback: status ? '' : 'Imported.' });
	observeSession.mock.calls[0][0]();
	expect(view.state()).toEqual({ likes: [], phase: 'error', feedback: '' });
	await view.read(new AbortController().signal, owner, 'old');
	view.switchBinding();
	effect.mock.calls[0][0]();
	expect(view.state()).toEqual({ likes: [], phase: 'loading', feedback: '' });
});

test.each([403, 412])('catalogue denial %s suppresses confirmed plays', async (status) => {
	const view = component('./RecentPlays.svelte', {
		ApiClient: class { async get() { throw new ApiError(status, 'denied', 'Denied'); } },
		loadRecentPlays: async () => [{ key: 'play', trackId: 'song', playedAt: new Date(0) }], recentPlayTitles: vi.fn()
	}, '{ load, state: () => view }');
	await view.load();
	expect(view.state()).toEqual({ kind: 'error' });
});

test('a rejected collection receipt clears the query and remains retryable', async () => {
	const view = component('./CollectionBrowser.svelte', {
		$props: () => ({ filter: 'all', runIfCurrent: async () => { throw new Error('No receipt'); } }),
		displayCollections: identity
	}, "{ load, search: () => { query = 'old'; }, state: () => ({ status, query, rows }) }");
	view.search();
	await view.load('all');
	expect(view.state()).toEqual({ status: 'unverified', query: '', rows: [] });
});

test('opening a stale or contradictory detail never enables retry', async () => {
	const row = { job_id: 'job', revision: 4, status: 'needs_review' };
	const detail = vi.fn();
	const view = component('../admin-ingest/IngestScreens.svelte', {
		...ingest, $props: () => ({ requests: { detail } }), tick: async () => {}
	}, '{ openJob, seed: (queuePage, heading) => { queue = queuePage; reviewHeading = heading; }, state: () => job }');
	view.seed({ jobs: [row] }, { focus: vi.fn() });
	await view.openJob('not-listed');
	expect(detail).not.toHaveBeenCalled();
	for (const invalid of [{ ...row, job_id: 'other' }, { ...row, revision: 3 }, { ...row, status: 'published' }]) {
		detail.mockResolvedValueOnce({ status: 200, body: invalid });
		await view.openJob(row.job_id);
		expect(view.state()).toBeUndefined();
	}
	detail.mockResolvedValueOnce({ status: 200, body: { ...row, revision: 5 } });
	await view.openJob(row.job_id);
	expect(view.state()?.revision).toBe(5);
});

test('owner suspension is set inside the synchronous page-hide flush', () => {
	const flushSync = vi.fn((work: () => void) => work());
	const view = component('../admin-ingest/PrivateIngest.svelte', {
		$props: () => ({ enabled: true, role: 'owner', requests: {} }), flushSync
	}, '{ onPageHide, state: () => suspended }');
	view.onPageHide();
	expect(flushSync).toHaveBeenCalledOnce();
	expect(view.state()).toBe(true);
});

test('changing the selection retires a download, and anchor failure reports refusal', () => {
	const release = vi.fn();
	const effect = vi.fn();
	const createElement = vi.fn(() => ({ remove: vi.fn() }));
	const view = component('./SelectionExport.svelte', {
		$effect: effect, document: { createElement }, createQueue: vi.fn(), appendSelection: () => ({ queue: { tracks: ['a'] } }),
		createM3UDownload: () => ({ release, download: () => 'Download started' }),
		formatQueueM3U: () => '#EXTM3U', m3uDownloadFailure: () => 'Could not start'
	}, "{ downloadSelectedM3U, select: () => { selectionKey = 'new'; }, message: () => status }");
	view.downloadSelectedM3U();
	effect.mock.calls[0][0]();
	expect(view.message()).toBe('Download started');
	release.mockClear();
	createElement.mockImplementationOnce(() => { throw new Error('Blocked'); });
	view.downloadSelectedM3U();
	expect(view.message()).toBe('Could not start');
	expect(release).toHaveBeenCalledOnce();
	view.downloadSelectedM3U();
	view.select();
	effect.mock.calls[0][0]();
	expect(release).toHaveBeenCalledTimes(2);
	expect(view.message()).toBe('');
});
