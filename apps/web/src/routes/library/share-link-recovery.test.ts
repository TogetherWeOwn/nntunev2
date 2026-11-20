import { afterEach, expect, test, vi } from 'vitest';
import ts from 'typescript';
import * as queueFunctions from '#lib/player/queue.js';
import { createQueueShare } from '#lib/player/queueShare.js';
import { createFavoritesExport } from './favoritesExport.js';
import { createLibraryRefresher } from '#lib/library/refresh.js';
import { clearFavoritesAtAuthBoundary, loadFavorites, selectFavoriteItems } from '#lib/library/favorites.js';
import { clearRecentFindsAtAuthBoundary, loadRecentFinds } from '#lib/library/recent-finds.js';
import { clearSignedInEmail } from '#lib/signed-in-email.js';
import { displayArtist, displayTitle, orderLibraryItems, shortTitle } from '#lib/library/order.js';
import { clearMiniPlayerHint } from '#lib/shell/mini-player.js';
import { createSelection, pruneSelection } from '#lib/library/selection.js';
import { resolveStreamUrl } from '#lib/library/stream-url.js';
import { resolveShareTarget } from '#lib/player/shareTimestamp.js';
import { createPlayerController } from '#lib/player/controller.svelte.js';
import { endTrackDrag } from '#lib/interactions/track-drag.js';
import { survivingInteractionIds } from './interactionSelection.js';

const sources = import.meta.glob('./+page.svelte', {
	query: '?raw', import: 'default', eager: true
}) as Record<string, string>;
const ITEMS = [{ id: 'coral', title: 'Coral Night', streamUrl: '/test-audio/coral' }];

/** Execute the real route script, refresher notifications and share helpers.
 * Only lifecycle/storage and Svelte state cells are substituted. Browser cases
 * in library-share-link-lifecycle.spec.ts cover navigation and rendered media.
 */
function pageScript(search: string, tick = () => Promise.resolve()) {
	vi.stubGlobal('window', { location: { search } });
	const source = sources['./+page.svelte'];
	const start = source.indexOf('>', source.indexOf('<script')) + 1;
	const script = source.slice(start, source.indexOf('</script', start));
	const parsed = ts.createSourceFile('library.ts', script, ts.ScriptTarget.Latest, true);
	// Presentation projections are lazy in Svelte and unused by these handlers.
	// Do not eagerly evaluate them as plain JavaScript: unrelated view imports
	// added by a base merge must not become dependencies of this state machine.
	const body = parsed.statements.filter((statement) => {
		if (ts.isImportDeclaration(statement)) return false;
		if (!ts.isVariableStatement(statement)) return true;
		return !statement.declarationList.declarations.some((declaration) => {
			if (declaration.name.getText(parsed) === 'queue') return false;
			const value = declaration.initializer;
			return value && ts.isCallExpression(value) &&
				(value.expression.getText(parsed) === '$derived' ||
				 value.expression.getText(parsed) === '$derived.by');
		});
	}).map((statement) => ts.isVariableStatement(statement) && statement.declarationList.declarations.some(
		(declaration) => declaration.name.getText(parsed) === 'queue'
	) ? 'let queue; queueController.subscribe((value) => { queue = value; });' : statement.getText(parsed)).join('\n');
	const compiled = ts.transpileModule(body, {
		compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ESNext }
	}).outputText;
	const bindings = {
		addTrack: queueFunctions.addTrack,
		clearQueue: queueFunctions.clearQueue,
		createQueue: queueFunctions.createQueue,
		createQueueShare,
		createPlayerController: (initial: queueFunctions.QueueState, end: Parameters<typeof createPlayerController>[1]) =>
			createPlayerController(initial, end, (audio, source) => audio.isConnected && audio.getAttribute('src') === source),
		createFavoritesExport,
		queueCurrentTrack: queueFunctions.currentTrack,
		selectQueueIndex: queueFunctions.selectTrack,
		insertTracks: queueFunctions.insertTracks, survivingInteractionIds,
		createLibraryRefresher, loadFavorites, selectFavoriteItems, loadRecentFinds,
		displayArtist, displayTitle, orderLibraryItems, createSelection, pruneSelection,
		clearMiniPlayerHint,
		clearSignedInEmail, clearFavoritesAtAuthBoundary, clearRecentFindsAtAuthBoundary,
		resolveStreamUrl, resolveShareTarget, endTrackDrag,
		browserStorage: () => null,
		invalidateLibraryCache: () => {},
		onMount: () => {},
		afterNavigate: () => {},
		shortTitle,
		tick,
		$props: () => ({}),
		$state: Object.assign(<T>(value: T) => value, { raw: <T>(value: T) => value }),
		$derived: <T>(value: T) => value
	};
	return new Function(...Object.keys(bindings), `${compiled}
		let renderedItems, playDisplayedDisabled = false;
		return {
			refresh, applyShareLink, handleLocalAuthTransition, selectTrack,
			requiresSignIn: () => refresher.state.requiresSignIn,
			attachNativeAudio, playQueuedTrack, subscribe: queueController.subscribe,
			addDisplayed: () => { renderedItems = items; addDisplayedTracksToQueue(); },
			source: queueController.sourceForQueuedPlay,
			endSession: refresher.handleLocalAuthTransition, dispose: refresher.dispose,
			setAudio: media.attach,
			currentAudio: () => media.audio,
			playTop: () => { renderedItems = items; return playDisplayedFromTop(); },
			playIds: () => playLibraryIds(['coral']),
			feedback: () => [player.queueAnnouncement.text, player.queueAnnouncement.version],
			read: () => ({ queue, shareInvalid, handledShareSearch, pendingShareSeek: media.pendingShareSeek, loadError })
		};
	`)(...Object.values(bindings)) as {
		refresh: () => Promise<void>;
		applyShareLink: () => void;
		handleLocalAuthTransition: () => void;
		requiresSignIn: () => boolean;
		selectTrack: (id: string) => void;
		attachNativeAudio: (element: EventTarget) => void;
		setAudio: (element: Pick<HTMLAudioElement, 'currentTime' | 'duration'> &
			Partial<Pick<HTMLAudioElement, 'isConnected' | 'getAttribute' | 'paused' | 'play' | 'pause'>>) => void;
		currentAudio: () => unknown;
		playTop: () => Promise<void>;
		playIds: () => Promise<void>;
		feedback: () => [string, number];
		playQueuedTrack: (id: string) => Promise<void>;
		addDisplayed: () => void;
		subscribe: (callback: (queue: queueFunctions.QueueState) => void) => () => void;
		source: ReturnType<typeof createPlayerController>['queue']['sourceForQueuedPlay'];
		endSession: () => void;
		dispose: () => void;
		read: () => {
			queue: queueFunctions.QueueState;
			shareInvalid: boolean;
			handledShareSearch: string | null;
			pendingShareSeek: { seconds: number } | null;
			loadError: string | null;
		};
	};
}

function playableAudio(play: () => Promise<void>, src = ITEMS[0].streamUrl) {
	return Object.assign(new EventTarget(), { currentTime: 0, duration: 60, paused: true, isConnected: true, getAttribute: () => src, play });
}

function serve(payload: { id: string; title: string; streamUrl?: string }[] = ITEMS) {
	vi.stubGlobal('fetch', async () => new Response(JSON.stringify(payload), {
		headers: { 'content-type': 'application/json' }
	}));
}

afterEach(() => vi.unstubAllGlobals());

for (const status of [500, 404]) {
	test(`a valid initial link survives an empty ${status} load and applies once on Retry`, async () => {
		vi.stubGlobal('fetch', async () => new Response(null, { status }));
		const page = pageScript('?track=coral&t=90');
		await page.refresh();
		expect(page.read().handledShareSearch).toBeNull();
		expect(page.read().shareInvalid).toBe(false);
		expect(page.read().queue.tracks).toEqual([]);

		serve();
		await page.refresh();
		expect(queueFunctions.currentTrack(page.read().queue)?.id).toBe('coral');
		expect(page.read().shareInvalid).toBe(false);
		expect(page.read().pendingShareSeek?.seconds).toBe(90);
		const audio = playableAudio(async () => {});
		page.attachNativeAudio(audio);
		audio.dispatchEvent(new Event('loadedmetadata'));
		expect(audio.currentTime).toBe(60);
		audio.currentTime = 12;
		await page.refresh();
		audio.dispatchEvent(new Event('durationchange'));
		expect(audio.currentTime).toBe(12);
	});
}

test('a valid same-route navigation while the empty load is failed survives Retry', async () => {
	vi.stubGlobal('fetch', async () => { throw new TypeError('synthetic network refusal'); });
	const page = pageScript('');
	await page.refresh();
	window.location.search = '?track=coral&t=9';
	page.applyShareLink();
	expect(page.read().handledShareSearch).toBeNull();
	expect(page.read().shareInvalid).toBe(false);
	serve();
	await page.refresh();
	expect(queueFunctions.currentTrack(page.read().queue)?.id).toBe('coral');
	expect(page.read().pendingShareSeek?.seconds).toBe(9);
	expect(page.read().shareInvalid).toBe(false);
});

for (const search of ['?track=missing&t=9', '?track=coral&t=bad']) {
	test(`Retry still rejects invalid parameters atomically: ${search}`, async () => {
		vi.stubGlobal('fetch', async () => new Response(null, { status: 500 }));
		const page = pageScript(search);
		await page.refresh();
		serve();
		await page.refresh();
		expect(page.read().shareInvalid).toBe(true);
		expect(page.read().handledShareSearch).toBe(search);
		expect(page.read().queue.tracks).toEqual([]);
		expect(page.read().pendingShareSeek).toBeNull();
	});
}

test('a successfully loaded empty library rejects the target and does not replay on Refresh', async () => {
	serve([]);
	const page = pageScript('?track=coral&t=9');
	await page.refresh();
	expect(page.read().shareInvalid).toBe(true);
	expect(page.read().handledShareSearch).toBe('?track=coral&t=9');
	serve();
	await page.refresh();
	expect(page.read().queue.tracks).toEqual([]);
});

test('refresh replaces source evidence without hydrating metadata until queued Play', async () => {
	serve();
	const page = pageScript('');
	await page.refresh();
	page.addDisplayed();
	const queued = page.read().queue;
	const old = page.source('coral');
	const updated = [{ ...ITEMS[0], streamUrl: '/test-audio/new' }];
	serve(updated);
	await page.refresh();
	expect(page.read().queue).toBe(queued);
	expect(queued.tracks[0].streamUrl).toBe('');
	expect(old?.isCurrent()).toBe(false);

	vi.stubGlobal('fetch', async () => new Response(null, { status: 503 }));
	const latest = page.source('coral');
	await page.refresh();
	expect(page.read().queue).toBe(queued);
	expect(latest?.isCurrent()).toBe(true);
	const play = vi.fn(async () => {});
	const attempt = page.playQueuedTrack('coral');
	page.setAudio(playableAudio(play, updated[0].streamUrl));
	await attempt;
	expect(page.read().queue.tracks[0].streamUrl).toBe(updated[0].streamUrl);
	expect(play).toHaveBeenCalledTimes(1);
});

test.each(['playTop', 'playIds'] as const)('%s hydrates before play and ignores an off-account rejection', async (gesture) => {
	serve();
	const page = pageScript('');
	await page.refresh();
	page.addDisplayed();
	expect(page.read().queue.tracks[0].streamUrl).toBe('');
	let rejectPlay!: (reason: Error) => void;
	const play = vi.fn(() => new Promise<void>((_resolve, reject) => { rejectPlay = reject; }));
	const attempt = page[gesture]();
	expect(queueFunctions.currentTrack(page.read().queue)?.streamUrl).toBe(ITEMS[0].streamUrl);
	expect(play).not.toHaveBeenCalled();
	page.setAudio(playableAudio(play));
	await Promise.resolve();
	expect(play).toHaveBeenCalledOnce();
	page.handleLocalAuthTransition();
	expect(page.read().queue.tracks).toEqual([]);
	const afterAuth = page.feedback();
	rejectPlay(new Error('blocked'));
	await attempt;
	expect(play).toHaveBeenCalledOnce();
	expect(page.feedback()).toEqual(afterAuth);
});

test('same-queue evidence replacement revokes an explicit Play waiting for flush', async () => {
	serve();
	let flush!: () => void;
	const tick = vi.fn(() => new Promise<void>((resolve) => { flush = resolve; }));
	const page = pageScript('', tick);
	await page.refresh();
	page.selectTrack('coral');
	const play = vi.fn(async () => {});
	const audio = playableAudio(play);
	page.setAudio(audio);
	const pending = page.playQueuedTrack('coral');
	expect(tick).toHaveBeenCalledOnce();
	const queue = page.read().queue;
	await page.refresh();
	expect(page.read().queue).toBe(queue);
	expect(page.currentAudio()).toBe(audio);
	flush();
	await pending;
	expect(play).not.toHaveBeenCalled();
});

for (const status of [200, 401, 403]) {
	test(`authoritative ${status} removal invalidates queued Play across a re-add`, async () => {
		serve();
		const page = pageScript('');
		await page.refresh();
		page.selectTrack('coral');
		const old = page.source('coral');
		vi.stubGlobal('fetch', async () => new Response(status === 200 ? '[]' : null, { status }));
		await page.refresh();
		expect(page.read().queue.tracks).toEqual([]);
		expect(page.source('coral')).toBeNull();
		serve([{ ...ITEMS[0], streamUrl: undefined }]);
		await page.refresh();
		expect(page.read().queue.tracks).toEqual([]);
		expect(page.source('coral')?.isCurrent()).toBe(true);
		expect(page.source('coral', ITEMS[0].streamUrl)?.streamUrl).toBe('');
		expect(old?.isCurrent()).toBe(false);
	});
}

for (const streamUrl of [undefined, '/test-audio/new']) {
	test(`queued Play retains its source when the refresh ${streamUrl ? 'changes' : 'omits'} the displayed URL`, async () => {
		serve();
		const page = pageScript('');
		await page.refresh();
		page.selectTrack('coral');
		const queue = page.read().queue;
		const old = page.source('coral');
		serve([{ ...ITEMS[0], streamUrl }]);
		await page.refresh();
		expect(page.read().queue).toBe(queue);
		expect(old?.isCurrent()).toBe(false);
		expect(page.source('coral')?.streamUrl).toBe(streamUrl ?? '');
		const play = vi.fn(async () => {});
		page.setAudio(playableAudio(play));
		await page.playQueuedTrack('coral');
		expect(page.read().queue.tracks[0].streamUrl).toBe(ITEMS[0].streamUrl);
		expect(play).toHaveBeenCalledOnce();
	});
}

test('queue publication exposes new evidence before a reentrant auth boundary', async () => {
	serve([...ITEMS, { id: 'kelp', title: 'Kelp', streamUrl: '/test-audio/kelp' }]);
	const page = pageScript('');
	await page.refresh();
	page.selectTrack('kelp');
	page.selectTrack('coral');
	let reentered = false;
	const stop = page.subscribe((queue) => {
		if (reentered || queue.tracks.length !== 1) return;
		reentered = true;
		expect(queue.tracks[0].streamUrl).toBe(ITEMS[0].streamUrl);
		expect(page.source('kelp')).toBeNull();
		expect(page.source('coral')?.streamUrl).toBe('/test-audio/new');
		const pause = vi.fn(() => {
			expect(page.currentAudio()).toBeUndefined();
			// Queue retirement alone would pause after the refresher changed state.
			expect(page.requiresSignIn()).toBe(false);
			expect(page.read().queue).toBe(queue);
		});
		page.setAudio(Object.assign(new EventTarget(), { currentTime: 0, duration: 60, paused: false, pause }));
		page.handleLocalAuthTransition();
		expect(pause).toHaveBeenCalledOnce();
		expect(page.currentAudio()).toBeUndefined();
		expect(page.requiresSignIn()).toBe(true);
	});
	serve([{ ...ITEMS[0], streamUrl: '/test-audio/new' }]);
	await page.refresh();
	stop();
	expect(reentered).toBe(true);
	expect(page.source('coral')).toBeNull();
	expect(page.read().queue.tracks).toEqual([]);
});

for (const boundary of ['endSession', 'dispose'] as const) {
	test(`late response cannot authorize a source after ${boundary}`, async () => {
		let respond!: (response: Response) => void;
		vi.stubGlobal('fetch', () => new Promise<Response>((resolve) => { respond = resolve; }));
		const page = pageScript('');
		const pending = page.refresh();
		page[boundary]();
		respond(new Response(JSON.stringify(ITEMS)));
		await pending;
		expect(page.source('coral')).toBeNull();
		expect(page.read().queue.tracks).toEqual([]);
	});
}

for (const status of [401, 403]) {
	test(`${status} invalidation still fences the link from the next account's Retry`, async () => {
		vi.stubGlobal('fetch', async () => new Response(null, { status }));
		const page = pageScript('?track=coral&t=9');
		await page.refresh();
		serve();
		await page.refresh();
		expect(page.read().shareInvalid).toBe(false);
		expect(page.read().queue.tracks).toEqual([]);
		expect(page.read().pendingShareSeek).toBeNull();
	});
}
