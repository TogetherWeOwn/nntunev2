import { fileURLToPath } from 'node:url';
import { defineConfig, mergeConfig, normalizePath } from 'vite';
import appConfig from '../vite.config';

const target = normalizePath(fileURLToPath(new URL('../src/routes/demo/player/+page.svelte', import.meta.url)));
const anchor = '\t\trefreshRecent();\n\t\ttry {\n\t\t\tqueue = restoreQueue';
const injection = `
		const listenFixture = new AbortController();
		const listenFixtureReady = new URLSearchParams(location.search).get('fixture') === 'listen-events'
			? import('../../../../tests/fixtures/listen-event-browser.js').then(({ mountListenEventFixture }) =>
				mountListenEventFixture(listenFixture.signal))
			: undefined;
		if (listenFixtureReady) window.__nntuneListenEventFixture = listenFixtureReady;
		const fixtureStore = recentStore;
		if (!fixtureStore) throw new Error('history fixture store not initialized');
		let fixtureActive = true;
		const fixture = {
			id: crypto.randomUUID(),
			async clear() {
				if (!fixtureActive || window.__nntuneHistoryFixture !== fixture || fixtureStore !== recentStore)
					throw new Error('history fixture mount inactive');
				fixtureStore.clear();
				refreshRecent();
				await tick();
			},
			entries() {
				if (!fixtureActive || window.__nntuneHistoryFixture !== fixture || fixtureStore !== recentStore)
					throw new Error('history fixture mount inactive');
				return fixtureStore.entries();
			}
		};
		window.__nntuneHistoryFixture = fixture;
`;
const teardown = `			fixtureActive = false;
			listenFixture.abort();
			if (listenFixtureReady && window.__nntuneListenEventFixture === listenFixtureReady)
				delete window.__nntuneListenEventFixture;
			if (window.__nntuneHistoryFixture === fixture) delete window.__nntuneHistoryFixture;
`;

export default defineConfig(async (env) => {
	let matched = false;
	return mergeConfig(await appConfig(env), {
		plugins: [{
			name: 'acceptance-history-live-store',
			enforce: 'pre',
			transform(code, id) {
				if (normalizePath(id) !== target) return;
				const cleanup = '\t\t\t// A copy settling after teardown must not resurrect state.';
				if (code.split(anchor).length !== 2 || code.split(cleanup).length !== 2)
					throw new Error('history fixture: route anchors missing or ambiguous');
				matched = true;
				return code.replace(anchor, `\t\trefreshRecent();\n${injection}\t\ttry {\n\t\t\tqueue = restoreQueue`)
					.replace(cleanup, teardown + cleanup);
			},
			buildEnd(error) {
				if (!error && !matched) throw new Error('history fixture: demo route not transformed');
			}
		}]
	});
});
