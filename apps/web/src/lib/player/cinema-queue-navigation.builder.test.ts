/** Execute the actual layout handler with controlled native-exit/navigation timing. */
import { expect, it, vi } from 'vitest';
import ts from 'typescript';

const sources = import.meta.glob('../../routes/+layout.svelte', { query: '?raw', import: 'default', eager: true }) as Record<string, string>;
const script = sources['../../routes/+layout.svelte'].split('<script lang="ts">')[1].split('</script>')[0];
const ast = ts.createSourceFile('layout.ts', script, ts.ScriptTarget.Latest, true);
const handler = ast.statements.find((node) => ts.isFunctionDeclaration(node) && node.name?.text === 'openQueue')!;
const compiled = ts.transpileModule(handler.getText(ast), { compilerOptions: { target: ts.ScriptTarget.ES2022 } }).outputText;

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
		let queueRoute = false, queueOpen = false, queueOpener = null, queueReturnUrl = '';
		let nowPlayingOpen = false, nowPlayingRoute = false, nowPlayingReturnUrl = '/home';
		let isPublicRoute = false, panelQuery = null;
		const page = { url: { pathname: '/home', search: '' } };
		const $player = { queue: {} };
		const currentTrack = () => ({ streamUrl: '/real-audio' });
		const queueHeading = { focus };
		const closeQueue = async () => { queueOpen = false; };
		${compiled}
		return {
			openQueue,
			navigate(path) { queueNavigationVersion++; page.url.pathname = path; queueRoute = path === '/queue'; },
			state: () => ({ queueOpen, queueOpener, queueReturnUrl })
		};
	`)(narrow, tick, focus, goto);
	goto.mockImplementation(async (path) => api.navigate(path));
	return { ...api, focus, goto };
}

it.each([true, false])('navigation supersedes a pending full-screen Queue gesture (narrow=%s)', async (narrow) => {
	const api = harness(narrow);
	const gate = deferred();
	const target = { isConnected: true };
	const pending = api.openQueue({ currentTarget: target, detail: 0 }, () => gate.promise);
	api.navigate('/library');
	gate.resolve(true);
	await pending;
	expect(api.goto).not.toHaveBeenCalled();
	expect(api.state().queueOpen).toBe(false);
	expect(api.focus).not.toHaveBeenCalled();
});

it('successful full-screen exit retains the original keyboard opener across the asynchronous boundary', async () => {
	const api = harness();
	const event = { currentTarget: { isConnected: true } as object | null, detail: 0 };
	const target = event.currentTarget;
	const gate = deferred();
	const pending = api.openQueue(event, () => gate.promise);
	event.currentTarget = null;
	gate.resolve(true);
	await pending;
	expect(api.goto).toHaveBeenCalledWith('/queue');
	expect(api.state().queueOpener).toBe(target);
	expect(api.focus).toHaveBeenCalledOnce();
});

it('a navigation during the post-open tick cancels obsolete queue heading focus', async () => {
	const gate = deferred();
	const api = harness(true, async () => { await gate.promise; });
	const pending = api.openQueue({ currentTarget: { isConnected: true }, detail: 0 }, async () => true);
	await vi.waitFor(() => expect(api.goto).toHaveBeenCalledOnce());
	api.navigate('/library');
	gate.resolve(true);
	await pending;
	expect(api.focus).not.toHaveBeenCalled();
});

it('a refused full-screen exit leaves queue presentation and focus unchanged', async () => {
	const api = harness();
	await api.openQueue({ currentTarget: { isConnected: true }, detail: 0 }, async () => false);
	expect(api.goto).not.toHaveBeenCalled();
	expect(api.state().queueOpen).toBe(false);
	expect(api.focus).not.toHaveBeenCalled();
});
