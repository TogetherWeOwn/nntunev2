/** Actual layout handlers: the compact identity link is a plain anchor, so
 * the layout only records its opener and the route close restores focus. */
import { expect, it, vi } from 'vitest';
import ts from 'typescript';

const sources = import.meta.glob('../../routes/+layout.svelte', { query: '?raw', import: 'default', eager: true }) as Record<string, string>;
const script = sources['../../routes/+layout.svelte'].split('<script lang="ts">')[1].split('</script>')[0];
const ast = ts.createSourceFile('layout.ts', script, ts.ScriptTarget.Latest, true);
const names = ['noteNowPlayingOpen', 'closeNowPlaying'];
const compiled = ast.statements.filter((node) => ts.isFunctionDeclaration(node) && names.includes(node.name?.text ?? ''))
	.map((node) => ts.transpileModule(node.getText(ast), { compilerOptions: { target: ts.ScriptTarget.ES2022 } }).outputText).join('\n');

function deferred() {
	let resolve!: (value: boolean) => void;
	const promise = new Promise<boolean>((yes) => { resolve = yes; });
	return { promise, resolve };
}

function harness(tick = async () => {}) {
	const focus = vi.fn();
	const goto = vi.fn();
	const api = new Function('tick', 'focus', 'goto', `
		let queueNavigationVersion = 0;
		let nowPlayingRoute = false, nowPlayingOpener = null, nowPlayingReturnUrl = '/library';
		let compactOpener = null, compactKeyboard = false;
		let isPublicRoute = false;
		const page = { url: new URL('https://app.test/library') };
		const document = { querySelector: () => ({ focus }) };
		${compiled}
		return {
			noteNowPlayingOpen, closeNowPlaying,
			navigate(path) {
				queueNavigationVersion++;
				page.url = new URL(path, page.url);
				nowPlayingRoute = page.url.pathname === '/now-playing';
			},
			invalidate(kind) { if (kind === 'auth') isPublicRoute = true; },
			state: () => ({ compactOpener, compactKeyboard, nowPlayingReturnUrl, path: page.url.pathname + page.url.search })
		};
	`)(tick, focus, goto);
	goto.mockImplementation(async (path) => api.navigate(path));
	return { ...api, focus, goto };
}

it('records the keyboard opener without navigating', () => {
	const api = harness();
	const target = { isConnected: true };
	api.noteNowPlayingOpen(target, true);
	expect(api.state().compactOpener).toBe(target);
	expect(api.state().compactKeyboard).toBe(true);
	expect(api.goto).not.toHaveBeenCalled();
});

it('pointer opens do not request heading focus', () => {
	const api = harness();
	api.noteNowPlayingOpen({ isConnected: true }, false);
	expect(api.state().compactKeyboard).toBe(false);
});

it('route close returns to the last non-Now-Playing URL and restores the opener', async () => {
	const api = harness();
	const target = { isConnected: true, focus: vi.fn() };
	api.noteNowPlayingOpen(target, true);
	api.navigate('/now-playing');
	await api.closeNowPlaying();
	expect(api.goto).toHaveBeenCalledWith('/library');
	expect(target.focus).toHaveBeenCalledOnce();
});

it('close falls back to the compact toggle when the opener is gone', async () => {
	const api = harness();
	api.noteNowPlayingOpen({ isConnected: false }, true);
	api.navigate('/now-playing');
	await api.closeNowPlaying();
	expect(api.focus).toHaveBeenCalledOnce();
});

it('new navigation during close retires opener focus', async () => {
	const gate = deferred(), api = harness(async () => { await gate.promise; });
	api.noteNowPlayingOpen({ isConnected: true }, true);
	api.navigate('/now-playing');
	const pending = api.closeNowPlaying();
	await vi.waitFor(() => expect(api.goto).toHaveBeenCalledOnce());
	api.navigate('/search'); gate.resolve(true); await pending;
	expect(api.focus).not.toHaveBeenCalled();
});

it('public routes never restore focus on close', async () => {
	const api = harness();
	api.invalidate('auth');
	api.navigate('/now-playing');
	await api.closeNowPlaying();
	expect(api.focus).not.toHaveBeenCalled();
});
