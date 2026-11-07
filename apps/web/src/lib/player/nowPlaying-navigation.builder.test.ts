/** Actual layout handlers under controlled full-screen, navigation and render timing. */
import { expect, it, vi } from 'vitest';
import ts from 'typescript';

const sources = import.meta.glob('../../routes/+layout.svelte', { query: '?raw', import: 'default', eager: true }) as Record<string, string>;
const script = sources['../../routes/+layout.svelte'].split('<script lang="ts">')[1].split('</script>')[0];
const ast = ts.createSourceFile('layout.ts', script, ts.ScriptTarget.Latest, true);
const names = ['openQueue', 'closeQueue', 'openNowPlaying', 'closeNowPlaying'];
const compiled = ast.statements.filter((node) => ts.isFunctionDeclaration(node) && names.includes(node.name?.text ?? ''))
	.map((node) => ts.transpileModule(node.getText(ast), { compilerOptions: { target: ts.ScriptTarget.ES2022 } }).outputText).join('\n');

function deferred() {
	let resolve!: (value: boolean) => void;
	const promise = new Promise<boolean>((yes) => { resolve = yes; });
	return { promise, resolve };
}

function harness(narrow = true, tick = async () => {}) {
	const focus = vi.fn();
	const goto = vi.fn();
	const api = new Function('narrow', 'tick', 'focus', 'goto', `
		let queueNavigationVersion = 0;
		let queueRoute = false, queueOpen = false, queueOpener = null, queueReturnUrl = '/home';
		let nowPlayingRoute = false, nowPlayingOpen = false, nowPlayingOpener = null, nowPlayingReturnUrl = '/home';
		let isPublicRoute = false, panelQuery = null, playable = true;
		const page = { url: new URL('https://app.test/library') };
		const $player = { queue: {} };
		const currentTrack = () => playable ? { streamUrl: '/real-audio' } : null;
		const queueHeading = { focus }, nowPlayingHeading = { focus };
		const document = { querySelector: () => ({ focus }) };
		${compiled}
		return {
			openQueue, closeQueue, openNowPlaying, closeNowPlaying,
			navigate(path) {
				queueNavigationVersion++;
				page.url = new URL(path, page.url);
				queueRoute = page.url.pathname === '/queue';
				nowPlayingRoute = page.url.pathname === '/now-playing';
				panelQuery = page.url.searchParams.get('panel');
			},
			invalidate(kind) { if (kind === 'auth') isPublicRoute = true; else playable = false; },
			state: () => ({ queueOpen, nowPlayingOpen, nowPlayingOpener, nowPlayingReturnUrl, queueReturnUrl, path: page.url.pathname + page.url.search })
		};
	`)(narrow, tick, focus, goto);
	goto.mockImplementation(async (path) => api.navigate(path));
	return { ...api, focus, goto };
}

function gesture() {
	return { currentTarget: { isConnected: true, focus: vi.fn() } as { isConnected: boolean; focus(): void } | null, detail: 0, button: 0, preventDefault: vi.fn() };
}

it.each([true, false])('new navigation retires a pending full-screen Now playing entry (narrow=%s)', async (narrow) => {
	const api = harness(narrow), gate = deferred();
	const pending = api.openNowPlaying(gesture(), () => gate.promise);
	api.navigate('/search'); gate.resolve(true); await pending;
	expect(api.goto).not.toHaveBeenCalled();
	expect(api.state().nowPlayingOpen).toBe(false);
	expect(api.focus).not.toHaveBeenCalled();
});

it.each(['auth', 'source', 'disconnect', 'refusal'])('%s retires a full-screen Now playing continuation', async (kind) => {
	const api = harness(), gate = deferred(), event = gesture();
	const pending = api.openNowPlaying(event, () => gate.promise);
	if (kind === 'disconnect') event.currentTarget!.isConnected = false;
	else if (kind !== 'refusal') api.invalidate(kind);
	gate.resolve(kind !== 'refusal'); await pending;
	expect(api.goto).not.toHaveBeenCalled();
	expect(api.state().nowPlayingOpen).toBe(false);
	expect(api.focus).not.toHaveBeenCalled();
});

it('retains original keyboard intent and target after the native-exit await', async () => {
	const api = harness(), gate = deferred(), event = gesture();
	const target = event.currentTarget;
	const pending = api.openNowPlaying(event, () => gate.promise);
	event.currentTarget = null; event.detail = 1;
	gate.resolve(true); await pending;
	expect(api.goto).toHaveBeenCalledWith('/now-playing');
	expect(api.state().nowPlayingOpener).toBe(target);
	expect(api.focus).toHaveBeenCalledOnce();
});

it('new navigation during the post-entry tick retires heading focus', async () => {
	const gate = deferred(), api = harness(true, async () => { await gate.promise; });
	const pending = api.openNowPlaying(gesture());
	await vi.waitFor(() => expect(api.goto).toHaveBeenCalledOnce());
	api.navigate('/search'); gate.resolve(true); await pending;
	expect(api.focus).not.toHaveBeenCalled();
});

it('new navigation during close retires opener focus', async () => {
	const gate = deferred(), api = harness(true, async () => { await gate.promise; });
	api.navigate('/now-playing');
	const pending = api.closeNowPlaying();
	await vi.waitFor(() => expect(api.goto).toHaveBeenCalledOnce());
	api.navigate('/search'); gate.resolve(true); await pending;
	expect(api.focus).not.toHaveBeenCalled();
});

it.each(['altKey', 'ctrlKey', 'metaKey', 'shiftKey', 'defaultPrevented'])('preserves native link behavior for %s', async (key) => {
	const api = harness(), event = { ...gesture(), [key]: true };
	await api.openNowPlaying(event);
	expect(event.preventDefault).not.toHaveBeenCalled();
	expect(api.goto).not.toHaveBeenCalled();
});

it('pointer entry does not steal heading focus', async () => {
	const api = harness(false);
	await api.openNowPlaying({ ...gesture(), detail: 1 });
	expect(api.state().nowPlayingOpen).toBe(true);
	expect(api.focus).not.toHaveBeenCalled();
});

it.each(['queue', 'now-playing'])('repeated switches starting with %s consume their return frame without a cycle', async (first) => {
	const api = harness();
	const openFirst = first === 'queue' ? api.openQueue : api.openNowPlaying;
	const openSecond = first === 'queue' ? api.openNowPlaying : api.openQueue;
	await openFirst(gesture()); await openSecond(gesture()); await openFirst(gesture());
	if (first === 'queue') await api.closeQueue(); else await api.closeNowPlaying();
	expect(api.state().path).toBe('/library');
});

it('query close preserves the main route, unrelated parameters and fragment', async () => {
	const api = harness(false);
	api.navigate('/search?panel=now-playing&keep=1#results');
	await api.closeNowPlaying();
	expect(api.goto.mock.calls[0][0].href).toBe('https://app.test/search?keep=1#results');
});
