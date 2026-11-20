import { json } from 'node:stream/consumers';
import { fileURLToPath } from 'node:url';
import { defineConfig, mergeConfig, normalizePath } from 'vite';
import appConfig from '../vite.config';

const route = (name: string) => normalizePath(fileURLToPath(new URL(`../src/routes/${name}/+page.svelte`, import.meta.url)));
const demo = route('demo/player');
const library = route('library');

function splice(code: string, anchor: string, replacement: string): string {
	if (code.split(anchor).length !== 2) throw new Error(`history fixture: missing or ambiguous anchor: ${anchor}`);
	return code.replace(anchor, () => replacement);
}

// Only the fixture build mounts the adapter. Production pages do not have
// an issued play identity yet, and these simulated grants are not staging proof.
const fixtureTransport = `<script lang="ts">
	import { createPageListenHistory } from '#lib/player/pageListenHistory.js';
	const listenHistory = createPageListenHistory();
	const fixtureGrants = new WeakMap<HTMLAudioElement, string>();
	function issueFixtureGrant(audio: HTMLAudioElement) {
		const bytes = crypto.getRandomValues(new Uint8Array(16));
		const id = btoa(String.fromCharCode(...bytes)).replaceAll('+', '-').replaceAll('/', '_').replace(/=+$/, '');
		fixtureGrants.set(audio, id);
		return () => (window as { __nntuneFixtureGrant?: string }).__nntuneFixtureGrant ?? fixtureGrants.get(audio);
	}`;
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
	const matched = new Set<string>();
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
				const target = normalizePath(id);
				if (target !== demo && target !== library) return;
				matched.add(target);
				code = splice(code, '<script lang="ts">', fixtureTransport);
				code = splice(code, '\tonMount(() => {\n', '\tonMount(() => {\n\t\tconst stopHistory = listenHistory.mount();\n');
				if (target === library) {
					code = splice(code, '\t\treturn media.attach(audio, {', '\t\tconst detach = media.attach(audio, {');
					const endAttach = '\t\t});\n\t}\n\n\tfunction announceEnd';
					code = splice(code, endAttach, `		});
		const stop = listenHistory.attach(audio, queueCurrentTrack(queueController.snapshot)?.id, issueFixtureGrant(audio));
		return () => { stop(); detach(); };
	}

	function announceEnd`);
					return splice(code, '\t\t\tmedia.destroy();', '\t\t\tstopHistory();\n\t\t\tmedia.destroy();');
				}
				const observe = '\t\tconst stopObserving = playback.observe(node);';
				code = splice(code, observe, `${observe}\n\t\tconst stop = listenHistory.attach(node, trackId, issueFixtureGrant(node));`);
				code = splice(code, '\t\t\t\tstopObserving();', '\t\t\t\tstop();\n\t\t\t\tstopObserving();');
				const finalNext = '\t\t\t// End of the queue: really stop, so status and playback agree.';
				code = splice(code, finalNext, `${finalNext}\n\t\t\tlistenHistory.finish(audioEl);\n\t\t\tif (audioEl) issueFixtureGrant(audioEl);`);
				const cleanup = '\t\t\t// A copy settling after teardown must not resurrect state.';
				return splice(splice(code, anchor, `\t\trefreshRecent();\n${injection}\t\ttry {\n\t\t\tqueue = restoreQueue`),
					cleanup, `\t\t\tstopHistory();\n${teardown}${cleanup}`);
			},
			buildEnd(error) {
				if (!error && matched.size !== 2) throw new Error('history fixture: player routes not transformed');
			}
		}]
	});
});
