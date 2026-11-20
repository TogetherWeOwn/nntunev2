import { json } from 'node:stream/consumers';
import { fileURLToPath } from 'node:url';
import { defineConfig, mergeConfig, normalizePath } from 'vite';
import appConfig from '../vite.config';

const target = normalizePath(fileURLToPath(new URL('../src/routes/demo/player/+page.svelte', import.meta.url)));
const anchor = '\t\trefreshRecent();\n\t\ttry {\n\t\t\tqueue = restoreQueue';
const injection = `
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
			if (window.__nntuneHistoryFixture === fixture) delete window.__nntuneHistoryFixture;
`;

export default defineConfig(async (env) => {
	let matched = false;
	return mergeConfig(await appConfig(env), {
		plugins: [{
			name: 'acceptance-history-live-store',
			enforce: 'pre',
			configurePreviewServer(server) {
				// Observe real keepalive delivery after reload, without a browser route interceptor.
				const events = new Map<string, unknown[]>();
				server.middlewares.use(async (req, res, next) => {
					const url = new URL(req.url ?? '/', 'http://localhost');
					if (url.pathname !== '/v1/plays/events') return next();
					res.setHeader('content-type', 'application/json');
					if (req.method === 'GET') return res.end(JSON.stringify(events.get(url.searchParams.get('playback_id') ?? '') ?? []));
					if (req.method !== 'POST' || !(Number(req.headers['content-length']) <= 1024)) return res.writeHead(400).end();
					const event = await json(req).catch(() => null) as { playback_id?: string } | null;
					if (typeof event?.playback_id !== 'string') return res.writeHead(400).end();
					events.set(event.playback_id, [...(events.get(event.playback_id) ?? []), event]);
					res.end('{}');
				});
			},
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
