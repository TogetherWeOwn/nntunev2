import { expect, test, vi } from 'vitest';
import ts from 'typescript';
import { addTrack, clearQueue, clearUpcoming, createQueue, currentTrack, keepCurrentAndUpcoming, moveTrack, removeTrack, selectTrack } from '#lib/player/queue.js';
import { resolveStreamUrl } from '#lib/library/stream-url.js';
import { displayArtist, displayTitle } from '#lib/library/order.js';
import { clearMiniPlayerHint, readMiniPlayerHint, setMiniPlayerHint } from '#lib/shell/mini-player.js';
import { resolveShareTarget } from '#lib/player/shareTimestamp.js';
import { createPlayerController } from '#lib/player/controller.js';
import { describeJump } from '#lib/player/relativeJump.js';
import { restartIsAvailable } from './restartAvailability.js';

const sources = import.meta.glob('./+page.svelte', {
	query: '?raw', import: 'default', eager: true
}) as Record<string, string>;

// Exercise the real page handlers and registered lifecycle with mutable native getters.
// Rendering is covered by the browser banks.
function restartFixture(tick: () => Promise<void> = () => Promise.resolve()) {
	const source = sources['./+page.svelte'];
	const script = source.slice(source.indexOf('>') + 1, source.indexOf('</script'));
	const parsed = ts.createSourceFile('library.ts', script, ts.ScriptTarget.Latest, true);
	const names = new Set([
		'queuedTrackFor', 'playableTrackFor', 'selectTrack', 'clearShareFeedback', 'replaceQueue', 'commitQueue', 'resetRestartOnSourceChange', 'syncMiniPlayerHint',
		'applyShareLink', 'handleSeeking', 'selectQueuedTrack', 'removeQueuedTrack',
		'moveQueuedTrack', 'clearQueuedTracks', 'trimPreviousTracks', 'trimUpcomingTracks',
		'refreshRestartAvailability', 'restartCurrentTrack', 'setRestartStatus', 'resetShareAtAuthBoundary', 'jumpRelative',
		'attachNativeAudio', 'announceEnd', 'onRepeatChange', 'shortTitle', 'playQueuedTrack', 'playSelected'
	]);
	const declarations = parsed.statements.filter(ts.isFunctionDeclaration).filter((node) =>
		node.name && names.has(node.name.text)
	);
	expect(new Set(declarations.map((node) => node.name?.text))).toEqual(names);
	const mount = parsed.statements.find((node) => ts.isExpressionStatement(node) &&
		ts.isCallExpression(node.expression) && node.expression.expression.getText(parsed) === 'onMount');
	const bindingsInRoute = new Set(['player', 'queueController', 'media']);
	const controllers = parsed.statements.filter((node) => ts.isVariableStatement(node) &&
		node.declarationList.declarations.some((declaration) => bindingsInRoute.has(declaration.name.getText(parsed))));
	if (!mount || controllers.length !== bindingsInRoute.size) throw new Error('Missing route mount or player authority');
	const handlers = [...declarations, ...controllers, mount].map((node) => node.getText(parsed)).join('\n');
	const compiled = ts.transpileModule(handlers, {
		compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ESNext }
	}).outputText;
	const track = { id: 'coral', title: 'Coral Night', streamUrl: '/test-audio/coral' };
	const location = { search: '' };
	const writes: number[] = [];
	const media = { position: 30, duration: 120, rangeStart: 0, refuse: false };
	class NativeAudio extends EventTarget {
		readonly listeners = new Map<string, EventListener>();
		override addEventListener(type: string, listener: EventListenerOrEventListenerObject | null, options?: boolean | AddEventListenerOptions) {
			if (typeof listener === 'function') this.listeners.set(type, listener);
			super.addEventListener(type, listener, options);
		}
		get duration() { return media.duration; }
		get currentTime() { return media.position; }
		set currentTime(value: number) {
			writes.push(value);
			if (media.refuse) throw new Error('native seek refused');
			media.position = value;
		}
		get seekable() {
			return { length: 1, start: () => media.rangeStart, end: () => media.duration };
		}
		sourceAttribute = track.streamUrl;
		getAttribute = (name: string) => name === 'src' ? this.sourceAttribute : null;
		connected = true;
		get isConnected() { return this.connected; }
		set isConnected(value: boolean) { this.connected = value; }
		ended = false; paused = true; error: Pick<MediaError, 'code' | 'message'> | null = null;
		volume = 0.6; muted = false; playbackRate = 1.25;
		play = () => Promise.resolve();
		pause() { this.paused = true; }
	}
	let element = new NativeAudio();
	const capture = (type: string) => {
		const target = element;
		const listener = target.listeners.get(type);
		if (!listener) throw new Error(`Missing native listener: ${type}`);
		return (currentTarget: unknown = target) => listener({ type, currentTarget } as Event);
	};
	let onClear = () => {};
	const bindings = {
		addTrack, clearQueue, clearUpcoming, createQueue, keepCurrentAndUpcoming, moveTrack,
		queueCurrentTrack: currentTrack, selectQueueIndex: selectTrack, removeTrack,
		resolveStreamUrl, displayArtist, displayTitle, resolveShareTarget, describeJump,
		clearMiniPlayerHint, setMiniPlayerHint, restartIsAvailable,
		createPlayerController: (_: ReturnType<typeof createQueue>, end: Parameters<typeof createPlayerController>[1]) => {
			const player = createPlayerController(createQueue([{ ...track, artist: '' }]), end, (audio, source) =>
				audio instanceof NativeAudio && audio.connected && audio.sourceAttribute === source);
			const clear = player.media.clearShareSeek;
			player.media.clearShareSeek = () => { onClear(); clear(); };
			return player;
		},
		tick, window: { location }, element, track, capture
	};
	const page = new Function(...Object.keys(bindings), `
		let items = [track], queue = createQueue(items), mediaRevision = 0;
		let loading = false, loadError = null, libraryUnavailable = false;
		let shareAlive = true, handledShareSearch = null;
		let shareInvalid = false, shareGeneration = 0, shareStatus = '', shareFallbackUrl = null;
		let queueConfirmGeneration = 0;
		let queueAnnouncement = '', restartCanSeek = false, restartStatus = '', restartStatusVersion = 0;
		let jumpStatus = '', jumpStatusVersion = 0, lastJumpSeek = null;
		let queueAnnouncementVersion = 0;
		let density, priorDocumentTitle = null, tabTitle = '';
		let cleanup;
		const onMount = (callback) => { cleanup = callback(); };
		const readStoredDensity = () => 'comfortable';
		const handleAuthTransition = () => {}, recordFindQuery = () => {};
		const subscribeLocalAuthTransition = () => () => {}, handleLocalAuthTransition = () => {};
		const clearSnapshot = () => {}, browserStorage = () => null, refresh = () => {};
		const endTrackDrag = () => {}, document = { title: '' };
		let queueExportAuthGeneration = 0;
		const queueShare = { dispose() {}, reset() {} }, favoritesExport = { destroy() {} }, refresher = { dispose() {} };
		window.addEventListener = () => {}; window.removeEventListener = () => {};
		${compiled}
		const library = queueController.prepareLibrary(items);
		queueController.publish(library.next, queueController.begin(library.next, 'replace', true), library.sources);
		queueController.subscribe((value) => { queue = value; });
		const queueRevision = queueController;
		return {
			detach: attachNativeAudio(element),
			restart: restartCurrentTrack, applyShareLink, retryShare: (type = 'loadedmetadata') => capture(type)(), jump: jumpRelative,
			resetAuth: resetShareAtAuthBoundary,
			select: selectQueuedTrack, remove: removeQueuedTrack, playQueued: playQueuedTrack,
			move: moveQueuedTrack, clear: clearQueuedTracks, cleanup,
			trimPrevious: trimPreviousTracks, trimUpcoming: trimUpcomingTracks,
			seeking: (target = element) => capture('seeking')(target), replaceQueue,
			subscribe: queueController.subscribe, attach: attachNativeAudio, currentAudio: () => media.audio,
			nativeIntent: () => ({ playing: media.attachment?.playing }),
			end: (mode) => { onRepeatChange({ currentTarget: { value: mode } }); return capture('ended')(); },
			refresh: () => capture('timeupdate')(),
			read: () => ({ restartCanSeek, restartStatus, restartStatusVersion, pendingShareSeek: media.pendingShareSeek, queue, jumpStatus, playEpoch: queueRevision.revision, mediaRevision, queueAnnouncement, queueAnnouncementVersion })
		};
	`)(...Object.values(bindings)) as {
		restart: () => void;
		resetAuth: () => void;
		applyShareLink: () => void;
		retryShare: (type?: 'loadedmetadata' | 'durationchange') => void;
		jump: (direction: 'back' | 'forward') => void;
		select: (index: number) => void;
		remove: (index: number) => void;
		move: (from: number, to: number) => void;
		clear: () => void;
		cleanup: () => void;
		trimPrevious: () => void;
		trimUpcoming: () => void;
		playQueued: (id: string) => Promise<void>;
		seeking: (target?: unknown) => void;
		replaceQueue: (queue: ReturnType<typeof createQueue>) => void;
		attach: (audio: NativeAudio) => () => void;
		detach: () => void;
		currentAudio: () => unknown;
		nativeIntent: () => { playing: boolean | undefined };
		subscribe: (callback: (queue: ReturnType<typeof createQueue>) => void) => () => void;
		end: (mode: 'off' | 'one' | 'queue') => Promise<void>;
		refresh: () => void;
		read: () => {
			restartCanSeek: boolean;
			restartStatus: string;
			restartStatusVersion: number;
			pendingShareSeek: unknown;
			queue: ReturnType<typeof createQueue>;
			jumpStatus: string;
			playEpoch: number;
			mediaRevision: number;
			queueAnnouncement: string;
			queueAnnouncementVersion: number;
		};
	};
	page.refresh();
	expect(page.read().restartCanSeek).toBe(true);
	const remount = (source = track.streamUrl) => {
		const next = new NativeAudio();
		next.sourceAttribute = source;
		page.attach(next);
		return element = next;
	};
	return { page, media, writes, location, track, element, capture, remount, observeClear: (callback: () => void) => { onClear = callback; } };
}

test('restart reads mutable availability once per gesture', () => {
	const { page, media, element, writes } = restartFixture();
	let reads = 0;
	Object.defineProperty(element, 'duration', { get: () => ++reads <= 2 ? 120 : NaN });
	page.restart();
	expect(reads).toBe(2);
	expect(writes).toEqual([0]);
	expect(media.position).toBe(0);
	expect(page.read().restartStatus).toBe('Restarted Coral Night.');
});

test('a throwing availability getter still supersedes a pending share', () => {
	const { page, media, location, element, writes } = restartFixture();
	media.duration = NaN;
	location.search = '?track=coral&t=45';
	page.applyShareLink();
	expect(page.read().pendingShareSeek).not.toBeNull();
	Object.defineProperty(element, 'duration', { get: () => { throw new Error('unreadable'); } });
	page.restart();
	expect(page.read().pendingShareSeek).toBeNull();
	expect(page.read().restartStatus).toBe('Could not restart Coral Night.');
	expect(writes).toEqual([]);
});

test('a throwing native source check cannot preserve a pending share', async () => {
	const { page, location, element, writes } = restartFixture();
	location.search = '?track=coral&t=45';
	page.applyShareLink();
	expect(page.read().pendingShareSeek).not.toBeNull();
	element.getAttribute = () => { throw new Error('unreadable source'); };
	page.restart();
	await Promise.resolve();
	expect(page.read().pendingShareSeek).toBeNull();
	expect(page.read().restartStatusVersion).toBe(0);
	expect(writes).toEqual([]);
});

test.each(['duration', 'range start'] as const)('restart stops availability reads after retirement during %s', (phase) => {
	const { page, element, remount, writes } = restartFixture();
	const readPosition = vi.fn(() => 30);
	const readEnd = vi.fn(() => 120);
	Object.defineProperties(element, {
		duration: { get() { if (phase === 'duration') remount(); return 120; } },
		currentTime: { get: readPosition },
		seekable: { get: () => ({ length: 1, start: () => { remount(); return 0; }, end: readEnd }) }
	});
	page.restart();
	expect(page.currentAudio()).not.toBe(element);
	expect(readPosition).toHaveBeenCalledTimes(phase === 'duration' ? 0 : 1);
	expect(readEnd).not.toHaveBeenCalled();
	expect(writes).toEqual([]);
	expect(page.read().restartStatusVersion).toBe(0);
});

test('a position getter cannot write through a reentrant source change', () => {
	const { page, element, writes } = restartFixture();
	page.replaceQueue(addTrack(page.read().queue, { id: 'pine', title: 'Pine', artist: '', streamUrl: '/test-audio/pine' }));
	let reads = 0;
	Object.defineProperty(element, 'currentTime', { get() {
		if (++reads === 2) page.select(1);
		return 30;
	}, set: (value: number) => writes.push(value) });
	page.restart();
	expect(currentTrack(page.read().queue)?.id).toBe('pine');
	expect(writes).toEqual([]);
	expect(page.read().restartStatusVersion).toBe(0);
});

test.each([false, true])('restart with a queue edit during assignment (newer jump: %s)', (newerJump) => {
	const { page, element, media } = restartFixture();
	page.jump('forward');
	expect(page.read().jumpStatus).toBe('Forward 10 seconds, now at 0:40.');
	const read = vi.fn(() => media.position);
	Object.defineProperty(element, 'currentTime', { get: read, set(value: number) {
		if (value === 0) page.replaceQueue(addTrack(page.read().queue, { id: 'pine', title: 'Pine', artist: '', streamUrl: '/test-audio/pine' }));
		media.position = value;
		if (value === 0 && newerJump) page.jump('forward');
		if (value === 0) read.mockClear();
	} });
	page.restart();
	expect(page.currentAudio()).toBe(element);
	expect(page.read().queue.tracks.map((track) => track.id)).toEqual(['coral', 'pine']);
	expect(media.position).toBe(newerJump ? 10 : 0);
	expect(page.read().jumpStatus).toBe(newerJump ? 'Forward 10 seconds, now at 0:10.' : '');
	// If queued, only the newer jump reads its starting position and readback.
	expect(read).toHaveBeenCalledTimes(newerJump ? 2 : 0);
	expect(page.read().restartStatusVersion).toBe(0);
});

test('a setter that crosses the account boundary cannot retire new jump feedback or read back', () => {
	const { page, element, remount, writes } = restartFixture();
	let reads = 0;
	Object.defineProperty(element, 'currentTime', { get: () => { reads += 1; return 30; }, set(value: number) {
		writes.push(value);
		page.resetAuth();
		remount();
		page.jump('forward');
	} });
	page.restart();
	expect(reads).toBe(2);
	expect(writes).toEqual([0, 40]);
	expect(page.read().jumpStatus).toBe('Forward 10 seconds, now at 0:40.');
	expect(page.read().restartCanSeek).toBe(false);
	expect(page.read().restartStatusVersion).toBe(0);
});

test('restart retires jump feedback before readback can start a newer same-source share', async () => {
	const { page, element, location, media, writes } = restartFixture();
	page.jump('forward');
	let jumpAtReadback: string | undefined;
	let reads = 0;
	Object.defineProperty(element, 'currentTime', { get() {
		if (++reads === 3) {
			jumpAtReadback = page.read().jumpStatus;
			location.search = '?track=coral&t=42';
			page.applyShareLink();
		}
		return media.position;
	}, set(value: number) { writes.push(value); media.position = value; } });
	page.restart();
	await Promise.resolve();
	expect(jumpAtReadback).toBe('');
	expect(writes).toEqual([40, 0, 42]);
	expect(page.read().restartStatusVersion).toBe(0);
});

test('a synchronous seeking event preserves the share retry; duration clamps it once', async () => {
	const { page, location, element, media, writes } = restartFixture();
	media.duration = NaN;
	Object.defineProperty(element, 'currentTime', {
		get: () => media.position,
		set: (value: number) => { writes.push(value); page.seeking(); media.position = value; }
	});
	location.search = '?track=coral&t=90';
	page.applyShareLink();
	await Promise.resolve();
	expect(page.read().pendingShareSeek).not.toBeNull();
	media.duration = 60;
	page.retryShare('durationchange');
	page.retryShare();
	expect(writes).toEqual([90, 60]);
	expect(page.read().pendingShareSeek).toBeNull();
});

test.each(['clamp', 'extend', 'restart', 'unknown'] as const)(
	'a duration event during the share setter is bounded and respects %s', async (outcome) => {
		const { page, location, element, media, writes } = restartFixture();
		media.duration = outcome === 'extend' ? 60 : NaN;
		Object.defineProperty(element, 'currentTime', {
			get: () => media.position,
			set(value: number) {
				writes.push(value);
				if (writes.length === 1 || outcome === 'unknown') {
					if (outcome !== 'unknown') media.duration = outcome === 'extend' ? 120 : 60;
					page.retryShare('durationchange');
					if (outcome === 'restart') page.restart();
				}
				media.position = value;
			}
		});
		location.search = '?track=coral&t=90';
		page.applyShareLink();
		await Promise.resolve();
		const expected = outcome === 'clamp' ? 60 : outcome === 'restart' ? 0 : 90;
		expect(writes).toEqual([outcome === 'extend' ? 60 : 90, expected]);
		expect(media.position).toBe(expected);
		if (outcome === 'unknown') expect(page.read().pendingShareSeek).not.toBeNull();
		else expect(page.read().pendingShareSeek).toBeNull();
	}
);

test.each(['reattach', 'auth', 'route', 'query', 'source'] as const)(
	'a metadata duration read cannot write after the share receipt loses %s', async (boundary) => {
		const { page, location, element, writes, remount } = restartFixture();
		location.search = '?track=coral&t=90';
		page.applyShareLink();
		Object.defineProperty(element, 'duration', { get: () => {
			switch (boundary) {
				case 'reattach': remount(); break;
				case 'auth': page.resetAuth(); break;
				case 'route': page.cleanup(); break;
				case 'query': location.search = '?track=coral&t=17'; break;
				case 'source': element.sourceAttribute = '/test-audio/other'; break;
			}
			return 60;
		} });
		page.retryShare();
		await Promise.resolve();
		expect(writes).toEqual([]);
		if (boundary === 'query') expect(page.currentAudio()).toBe(element);
	}
);

test('a share tick cannot seek a same-source replacement mounted before the tick', async () => {
	const { page, location, remount, writes } = restartFixture();
	location.search = '?track=coral&t=90';
	page.applyShareLink();
	remount();
	await Promise.resolve();
	page.retryShare();
	expect(writes).toEqual([]);
	expect(page.read().pendingShareSeek).toBeNull();
});

test.each(['tick', 'loadedmetadata', 'durationchange'] as const)(
	'%s cannot seek a same-source remount using a share retained after detach', async (retry) => {
		let resume!: () => void;
		const { page, element, location, remount, writes } = restartFixture(() => new Promise<void>((resolve) => { resume = resolve; }));
		expect(page.currentAudio()).toBe(element);
		location.search = '?track=coral&t=90';
		page.applyShareLink();
		const pending = page.read().pendingShareSeek;
		expect(pending).toMatchObject({ trackId: 'coral', streamUrl: element.sourceAttribute, seconds: 90 });
		page.detach();
		expect(page.currentAudio()).toBeUndefined();
		expect(page.read().pendingShareSeek).toBe(pending);
		const replacement = remount();
		expect(page.currentAudio()).toBe(replacement);
		expect(replacement.sourceAttribute).toBe(element.sourceAttribute);
		expect(page.read().pendingShareSeek).toBe(pending);
		if (retry === 'tick') resume();
		else replacement.dispatchEvent(new Event(retry));
		await Promise.resolve();
		expect(writes).toEqual([]);
	}
);

test('a new same-source share navigation supersedes an earlier restart marker', async () => {
	const { page, media, location } = restartFixture();
	page.restart();
	expect(media.position).toBe(0);
	location.search = '?track=coral&t=42';
	page.applyShareLink();
	await Promise.resolve();
	expect(media.position).toBe(42);
	expect(page.read().pendingShareSeek).toBeNull();
	expect(page.read().queue.tracks.map((track) => track.id)).toEqual(['coral']);
});

test('direct queue selection retires a pending share adjustment across a round trip', async () => {
	const { page, media, location, writes, remount } = restartFixture();
	page.replaceQueue(addTrack(page.read().queue, { id: 'pine', title: 'Pine', artist: '', streamUrl: '/test-audio/pine' }));
	media.duration = Number.NaN;
	location.search = '?track=coral&t=45';
	page.applyShareLink();
	await Promise.resolve();
	expect(writes).toEqual([45]);
	expect(page.read().pendingShareSeek).not.toBeNull();

	page.select(0);
	expect(page.read().pendingShareSeek).not.toBeNull();
	page.select(-1);
	expect(page.read().pendingShareSeek).not.toBeNull();
	page.select(1);
	expect(page.read().pendingShareSeek).toBeNull();
	page.select(0);
	remount();
	media.position = 12;
	media.duration = 20;
	page.retryShare();
	expect(media.position).toBe(12);
	expect(writes).toEqual([45]);
});

test.each(['select', 'replace'] as const)('%s clears the pending transaction before publishing a new source', (mode) => {
	const { page, media, location, observeClear } = restartFixture();
	page.replaceQueue(addTrack(page.read().queue, { id: 'pine', title: 'Pine', artist: '', streamUrl: '/test-audio/pine' }));
	media.duration = Number.NaN;
	location.search = '?track=coral&t=45';
	page.applyShareLink();
	let sourceAtClear: string | undefined;
	observeClear(() => { sourceAtClear = currentTrack(page.read().queue)?.id; });
	if (mode === 'select') page.select(1);
	else page.replaceQueue(selectTrack(page.read().queue, 1));
	expect(sourceAtClear).toBe('coral');
	expect(page.read().pendingShareSeek).toBeNull();
	expect(currentTrack(page.read().queue)?.id).toBe('pine');
});

test('same-source replacement preserves pending share but only a new object fences playback', () => {
	const { page, media, location } = restartFixture();
	media.duration = Number.NaN;
	location.search = '?track=coral&t=45';
	page.applyShareLink();
	const before = page.read();
	expect(before.pendingShareSeek).not.toBeNull();
	page.replaceQueue(before.queue);
	expect(page.read().playEpoch).toBe(before.playEpoch);
	expect(page.read().pendingShareSeek).toBe(before.pendingShareSeek);
	page.replaceQueue({ ...before.queue });
	expect(page.read().playEpoch).toBe(before.playEpoch + 1);
	expect(page.read().mediaRevision).toBe(before.mediaRevision);
	expect(page.read().pendingShareSeek).toBe(before.pendingShareSeek);
});

test('a new URL for the same ID retires the attachment before queue publication', () => {
	const { page, element, remount } = restartFixture();
	const original = page.read().queue;
	const next = createQueue(original.tracks.map((track) => ({ ...track, streamUrl: '/test-audio/new' })));
	const observed: unknown[] = [];
	const unsubscribe = page.subscribe(() => observed.push(page.currentAudio()));
	page.replaceQueue(next);
	page.replaceQueue(original);
	expect(observed).toEqual([element, undefined, undefined]);
	expect(page.read().mediaRevision).toBe(2);
	const replacement = remount();
	page.detach();
	expect(page.currentAudio()).toBe(replacement);
	unsubscribe();
});

test('queued and captured events from a retired node cannot change its same-source replacement', async () => {
	const { page, element, media, capture, remount } = restartFixture();
	const [play, pause, error, ended, seeking, progress] =
		['play', 'pause', 'error', 'ended', 'seeking', 'progress'].map(capture);
	const queued: Event[] = [];
	element.paused = false;
	element.pause = () => { element.paused = true; queued.push(new Event('timeupdate'), new Event('pause')); };
	page.detach();
	const replacement = remount();
	page.detach();
	element.paused = replacement.paused = false;
	play();
	expect(page.nativeIntent().playing).toBe(false);
	replacement.dispatchEvent(new Event('playing'));
	page.jump('forward');
	const before = page.nativeIntent();
	expect(before.playing).toBe(true);
	expect(() => page.attach(element)).toThrow(/fresh audio element/);
	expect(page.currentAudio()).toBe(replacement);
	element.paused = replacement.paused = true;
	pause();
	element.error = { code: 3, message: 'decode failed' };
	error();
	element.error = null;
	media.position = 80;
	media.duration = NaN;
	seeking();
	progress();
	expect(queued).toHaveLength(2);
	for (const event of queued) element.dispatchEvent(event);
	element.ended = true;
	ended();
	expect(page.nativeIntent()).toEqual(before);
	expect(page.read().jumpStatus).toBe('Forward 10 seconds, now at 0:40.');
	expect(page.read().restartCanSeek).toBe(true);
	replacement.dispatchEvent(new Event('timeupdate'));
	expect(page.read().restartCanSeek).toBe(false);
	replacement.dispatchEvent(new Event('pause'));
	expect(page.nativeIntent()).toEqual({ playing: false });
	replacement.ended = true;
	await page.end('one');
	expect(media.position).toBe(80);
	expect(page.read().queueAnnouncement).toBe('');
});

test('a reentrant subscription cannot publish an older operation hint', () => {
	const { page } = restartFixture();
	const pine = createQueue([{ id: 'pine', title: 'Pine', artist: '', streamUrl: '/test-audio/pine' }]);
	const oak = createQueue([{ id: 'oak', title: 'Oak', artist: '', streamUrl: '/test-audio/oak' }]);
	const unsubscribe = page.subscribe((value) => {
		if (value === pine) page.replaceQueue(oak);
	});
	page.replaceQueue(pine);
	expect(page.read().queue).toBe(oak);
	expect(readMiniPlayerHint()?.trackId).toBe('oak');
	unsubscribe();
});

test('reentrant replacement during share append prevents the older navigation', async () => {
	const { page, location, writes } = restartFixture();
	page.replaceQueue(createQueue([]));
	const newer = selectTrack(createQueue([
		{ id: 'coral', title: 'Coral Night', artist: '', streamUrl: '/test-audio/coral' },
		{ id: 'pine', title: 'Pine', artist: '', streamUrl: '/test-audio/pine' }
	]), 1);
	const unsubscribe = page.subscribe((value) => {
		if (value.tracks.length === 1 && value.tracks[0].id === 'coral') page.replaceQueue(newer);
	});
	location.search = '?track=coral&t=45';
	page.applyShareLink();
	await Promise.resolve();
	expect(page.read().queue).toBe(newer);
	expect(page.read().pendingShareSeek).toBeNull();
	expect(writes).toEqual([]);
	unsubscribe();
});

test('reentrant replacement suppresses the superseded removal announcement', () => {
	const { page } = restartFixture();
	const pine = createQueue([{ id: 'pine', title: 'Pine', artist: '', streamUrl: '/test-audio/pine' }]);
	const version = page.read().queueAnnouncementVersion;
	const unsubscribe = page.subscribe((value) => {
		if (value.tracks.length === 0) page.replaceQueue(pine);
	});
	page.remove(0);
	expect(page.read().queue).toBe(pine);
	expect(page.read().queueAnnouncement).toBe('');
	expect(page.read().queueAnnouncementVersion).toBe(version);
	unsubscribe();
});

test('reentrant replacement prevents the superseded play attempt', async () => {
	const { page, element } = restartFixture();
	const pine = createQueue([{ id: 'pine', title: 'Pine', artist: '', streamUrl: '/test-audio/pine' }]);
	let plays = 0;
	element.play = () => { plays += 1; return Promise.resolve(); };
	const original = page.read().queue;
	const unsubscribe = page.subscribe((value) => {
		if (value !== original && value !== pine) page.replaceQueue(pine);
	});
	await page.playQueued('coral');
	expect(page.read().queue).toBe(pine);
	expect(plays).toBe(0);
	expect(page.read().queueAnnouncement).toBe('');
	expect(page.read().queueAnnouncementVersion).toBe(0);
	unsubscribe();
});

test('a clear subscriber invoking the share handler leaves the share selection and pending seek final', () => {
	const { page, media, location } = restartFixture();
	media.duration = Number.NaN;
	location.search = '?track=coral&t=45';
	let reacted = 0;
	const unsubscribe = page.subscribe((value) => {
		if (value.tracks.length === 0) { reacted += 1; page.applyShareLink(); }
	});
	const version = page.read().queueAnnouncementVersion;
	page.clear();
	const after = page.read();
	expect(reacted).toBe(1);
	expect(after.queue.tracks.map((track) => track.id)).toEqual(['coral']);
	expect(currentTrack(after.queue)?.id).toBe('coral');
	expect(after.pendingShareSeek).toMatchObject({ trackId: 'coral', streamUrl: '/test-audio/coral', seconds: 45 });
	expect(after.queueAnnouncement).toBe('');
	expect(after.queueAnnouncementVersion).toBe(version);
	unsubscribe();
});

test('a selection subscriber replacing with the same object delivers once and still starts playback', async () => {
	const { page, remount } = restartFixture();
	const pine = { id: 'pine', title: 'Pine', artist: '', streamUrl: '/test-audio/pine' };
	page.replaceQueue({ tracks: [pine, { id: 'coral', title: 'Coral Night', artist: '', streamUrl: '' }], currentIndex: 0 });
	let plays = 0, deliveries = 0, replaced = 0;
	const unsubscribe = page.subscribe((value) => {
		deliveries += 1;
		if (value.currentIndex === 1) { replaced += 1; page.replaceQueue(value); }
	});
	const attempt = page.playQueued('coral');
	remount().play = () => { plays += 1; return Promise.resolve(); };
	await attempt;
	// One initial delivery plus the single selection; the same-object replace publishes nothing.
	expect(deliveries).toBe(2);
	expect(replaced).toBe(1);
	expect(currentTrack(page.read().queue)?.id).toBe('coral');
	expect(plays).toBe(1);
	expect(page.read().queueAnnouncement).toBe('Playing Coral Night.');
	expect(page.read().queueAnnouncementVersion).toBe(1);
	unsubscribe();
});

test.each(['move', 'remove', 'clear', 'select', 'trimPrevious', 'trimUpcoming'] as const)(
	'%s publishes its real caller policy', (mode) => {
		const { page, media, location, track } = restartFixture();
		const pine = { id: 'pine', title: 'Pine', artist: '', streamUrl: '/test-audio/pine' };
		const oak = { id: 'oak', title: 'Oak', artist: '', streamUrl: '/test-audio/oak' };
		page.replaceQueue({ tracks: [pine, { ...track, artist: '' }, oak], currentIndex: 1 });
		media.duration = Number.NaN;
		location.search = '?track=coral&t=45';
		page.applyShareLink();
		const before = page.read();
		expect(before.pendingShareSeek).not.toBeNull();
		if (mode === 'move') page.move(0, 2);
		if (mode === 'remove') page.remove(1);
		if (mode === 'clear') page.clear();
		if (mode === 'select') page.select(2);
		if (mode === 'trimPrevious') page.trimPrevious();
		if (mode === 'trimUpcoming') page.trimUpcoming();
		const after = page.read();
		expect(after.playEpoch).toBe(before.playEpoch + 1);
		expect(after.queueAnnouncementVersion).toBe(before.queueAnnouncementVersion + (mode === 'remove' || mode === 'clear' ? 1 : 0));
		expect(after.queueAnnouncement).not.toBe('');
		expect(after.pendingShareSeek === null).toBe(mode === 'select');
		expect(after.restartStatus).toBe(before.restartStatus);
		expect(after.jumpStatus).toBe(before.jumpStatus);
		const current = currentTrack(after.queue);
		expect(current?.id ?? null).toBe(mode === 'clear' ? null : mode === 'remove' || mode === 'select' ? 'oak' : 'coral');
		expect(readMiniPlayerHint()?.title ?? null).toBe(current?.title ?? null);
	}
);

test.each(['move', 'remove', 'clear', 'select', 'trimPrevious', 'trimUpcoming'] as const)(
	'%s preserves the established restart and jump policy', (mode) => {
		const { page, track } = restartFixture();
		const pine = { id: 'pine', title: 'Pine', artist: '', streamUrl: '/test-audio/pine' };
		const oak = { id: 'oak', title: 'Oak', artist: '', streamUrl: '/test-audio/oak' };
		page.replaceQueue({ tracks: [pine, { ...track, artist: '' }, oak], currentIndex: 1 });
		page.restart();
		page.jump('forward');
		const before = page.read();
		expect(before.restartStatus).not.toBe('');
		expect(before.jumpStatus).not.toBe('');
		if (mode === 'move') page.move(0, 2);
		if (mode === 'remove') page.remove(1);
		if (mode === 'clear') page.clear();
		if (mode === 'select') page.select(2);
		if (mode === 'trimPrevious') page.trimPrevious();
		if (mode === 'trimUpcoming') page.trimUpcoming();
		const resets = mode === 'remove' || mode === 'select';
		expect(page.read().restartStatus).toBe(resets ? '' : before.restartStatus);
		expect(page.read().jumpStatus).toBe(resets ? '' : before.jumpStatus);
	}
);

test('a queue edit before tick prevents native play and stale feedback', async () => {
	let resume!: () => void;
	const { page, element } = restartFixture(() => new Promise<void>((resolve) => { resume = resolve; }));
	let plays = 0;
	element.play = () => { plays += 1; return Promise.resolve(); };
	const attempt = page.playQueued('coral');
	page.remove(0);
	const edited = page.read();
	resume();
	await attempt;
	expect(plays).toBe(0);
	expect(page.read().queueAnnouncement).toBe(edited.queueAnnouncement);
	expect(page.read().queueAnnouncementVersion).toBe(edited.queueAnnouncementVersion);
});

test('native play access cannot start an attempt retired by a queue edit', async () => {
	const { page, element } = restartFixture();
	let plays = 0;
	let afterEdit: ReturnType<typeof page.read> | undefined;
	Object.defineProperty(element, 'play', { get() {
		page.remove(0);
		afterEdit = page.read();
		return () => { plays += 1; return Promise.resolve(); };
	} });
	await page.playQueued('coral');
	expect(plays).toBe(0);
	expect(page.read().queue.tracks).toEqual([]);
	expect(page.read()).toEqual(afterEdit);
});

test.each(['detached', 'wrong source'] as const)('queued Play refuses a %s attachment', async (target) => {
	const { page, element } = restartFixture();
	element.isConnected = target !== 'detached';
	if (target === 'wrong source') element.getAttribute = () => '/test-audio/pine';
	const play = vi.spyOn(element, 'play');
	await page.playQueued('coral');
	expect(play).not.toHaveBeenCalled();
	expect(currentTrack(page.read().queue)?.id).toBe('coral');
	expect(page.read().queueAnnouncement).toBe('Selected Coral Night, paused: playback was blocked.');
	expect(page.read().queueAnnouncementVersion).toBe(1);
});

test('a late rejection cannot report against a replacement attachment at the same source', async () => {
	const { page, element, remount } = restartFixture();
	let rejectPlay!: (reason: Error) => void;
	element.play = () => new Promise<void>((_resolve, reject) => { rejectPlay = reject; });
	const attempt = page.playQueued('coral');
	await Promise.resolve();
	const replacement = remount();
	replacement.paused = false;
	replacement.dispatchEvent(new Event('playing'));
	expect(page.nativeIntent().playing).toBe(true);
	const before = page.read();
	rejectPlay(new Error('blocked'));
	await attempt;
	expect(page.currentAudio()).toBe(replacement);
	expect(page.nativeIntent().playing).toBe(true);
	expect(page.read()).toEqual(before);
});

test('route cleanup fences an explicit play waiting for tick', async () => {
	let resume!: () => void;
	const { page, element } = restartFixture(() => new Promise<void>((resolve) => { resume = resolve; }));
	let plays = 0;
	element.play = () => { plays += 1; return Promise.resolve(); };
	const attempt = page.playQueued('coral');
	page.cleanup();
	const afterCleanup = page.read();
	resume();
	await attempt;
	expect(plays).toBe(0);
	expect(page.read().queueAnnouncement).toBe(afterCleanup.queueAnnouncement);
	expect(page.read().queueAnnouncementVersion).toBe(afterCleanup.queueAnnouncementVersion);
});

test.each(['resolve', 'reject'] as const)('route cleanup fences deferred play %s', async (outcome) => {
	const { page, element } = restartFixture();
	let plays = 0;
	let settle!: () => void;
	const deferredPlay = new Promise<void>((resolve, reject) => {
		settle = () => outcome === 'resolve' ? resolve() : reject(new Error('blocked'));
	});
	element.play = () => { plays += 1; return deferredPlay; };
	const attempt = page.playQueued('coral');
	await Promise.resolve();
	expect(plays).toBe(1);
	page.cleanup();
	const afterCleanup = page.read();
	settle();
	await attempt;
	expect(plays).toBe(1);
	expect(page.read().queueAnnouncement).toBe(afterCleanup.queueAnnouncement);
	expect(page.read().queueAnnouncementVersion).toBe(afterCleanup.queueAnnouncementVersion);
});

test('an old share tick cannot apply a newer same-source share', async () => {
	const resumes: Array<() => void> = [];
	const { page, location, writes } = restartFixture(() => new Promise<void>((resolve) => { resumes.push(resolve); }));
	location.search = '?track=coral&t=45';
	page.applyShareLink();
	location.search = '?track=coral&t=60';
	page.applyShareLink();
	resumes[0]();
	await Promise.resolve();
	expect(writes).toEqual([]);
	resumes[1]();
	await Promise.resolve();
	expect(writes).toEqual([60]);
});

test.each(['resolve', 'reject'] as const)('a queue edit fences deferred play %s', async (outcome) => {
	const { page, element } = restartFixture();
	let settle!: () => void;
	element.play = () => new Promise<void>((resolve, reject) => {
		settle = () => outcome === 'resolve' ? resolve() : reject(new Error('blocked'));
	});
	const attempt = page.playQueued('coral');
	await Promise.resolve();
	page.remove(0);
	const edited = page.read();
	settle();
	await attempt;
	expect(page.read().queueAnnouncement).toBe(edited.queueAnnouncement);
	expect(page.read().queueAnnouncementVersion).toBe(edited.queueAnnouncementVersion);
});

test.each(['resolve', 'reject'] as const)('same-object replacement preserves deferred play %s', async (outcome) => {
	const { page, element } = restartFixture();
	element.paused = false;
	element.dispatchEvent(new Event('playing'));
	expect(page.nativeIntent().playing).toBe(true);
	let settle!: () => void;
	element.play = () => new Promise<void>((resolve, reject) => {
		settle = () => outcome === 'resolve' ? resolve() : reject(new Error('blocked'));
	});
	const attempt = page.playQueued('coral');
	await Promise.resolve();
	page.replaceQueue(page.read().queue);
	const version = page.read().queueAnnouncementVersion;
	settle();
	await attempt;
	expect(page.read().queueAnnouncement).toBe(outcome === 'resolve' ? 'Playing Coral Night.' : 'Selected Coral Night, paused: playback was blocked.');
	expect(page.nativeIntent().playing).toBe(outcome === 'resolve');
	expect(page.read().queueAnnouncementVersion).toBe(version + 1);
	expect(currentTrack(page.read().queue)?.id).toBe('coral');
});

test('same-source new-object replacement fences a deferred play rejection', async () => {
	const { page, element } = restartFixture();
	let rejectPlay!: (reason: Error) => void;
	element.play = () => new Promise<void>((_resolve, reject) => { rejectPlay = reject; });
	const attempt = page.playQueued('coral');
	await Promise.resolve();
	page.replaceQueue({ ...page.read().queue });
	const afterEdit = page.read();
	rejectPlay(new Error('blocked'));
	await attempt;
	expect(page.read().queueAnnouncement).toBe(afterEdit.queueAnnouncement);
	expect(page.read().queueAnnouncementVersion).toBe(afterEdit.queueAnnouncementVersion);
});

test('a jump readout clears on a same-source share and on a source change', async () => {
	const { page, media, location } = restartFixture();
	page.jump('forward');
	expect(media.position).toBe(40);
	expect(page.read().jumpStatus).toBe('Forward 10 seconds, now at 0:40.');
	location.search = '?track=coral&t=90';
	page.applyShareLink();
	await Promise.resolve();
	expect(media.position).toBe(90);
	expect(page.read().jumpStatus).toBe('');
	page.jump('back');
	page.replaceQueue(addTrack(page.read().queue, { id: 'pine', title: 'Pine', artist: '', streamUrl: '/test-audio/pine' }));
	page.select(1);
	expect(page.read().jumpStatus).toBe('');
});

test('removing the current queue row clears its jump readout', () => {
	const { page } = restartFixture();
	page.replaceQueue(addTrack(page.read().queue, { id: 'pine', title: 'Pine', artist: '', streamUrl: '/test-audio/pine' }));
	page.jump('forward');
	page.remove(0);
	expect(currentTrack(page.read().queue)?.id).toBe('pine');
	expect(page.read().jumpStatus).toBe('');
});

test('native seeking clears a previous jump readout but not the jump seek itself', () => {
	const { page, media } = restartFixture();
	page.jump('forward');
	page.seeking();
	expect(page.read().jumpStatus).toBe('Forward 10 seconds, now at 0:40.');
	media.position = 90;
	page.seeking();
	expect(page.read().jumpStatus).toBe('');
});

test('repeated seeks at the jump target retain an accurate readout', () => {
	const { page } = restartFixture();
	page.jump('forward');
	page.seeking();
	page.seeking();
	expect(page.read().jumpStatus).toBe('Forward 10 seconds, now at 0:40.');
});

test('detached media events cannot retire the current jump announcement', () => {
	const { page } = restartFixture();
	page.jump('forward');
	page.seeking({});	page.seeking();
	expect(page.read().jumpStatus).toBe('Forward 10 seconds, now at 0:40.');
});

test('an older jump event preserves newer no-write boundary feedback', () => {
	for (const direction of ['forward', 'back'] as const) {
		const { page, media, writes } = restartFixture();
		media.position = direction === 'forward' ? 115 : 5;
		page.jump(direction);
		expect(media.position).toBe(direction === 'forward' ? 120 : 0);
		page.jump(direction);
		const boundary = `Already at the ${direction === 'forward' ? 'end' : 'start'}.`;
		expect(page.read().jumpStatus).toBe(boundary);
		expect(writes).toHaveLength(1);
		// Deliver the first jump's queued event only after the no-write outcome.
		page.seeking();
		expect(page.read().jumpStatus).toBe(boundary);
		media.position = 60;
		page.seeking();
		expect(page.read().jumpStatus).toBe('');
	}
});

test('a native seek retires earlier boundary feedback', () => {
	const { page, media } = restartFixture();
	media.position = 0;
	page.jump('back');
	expect(page.read().jumpStatus).toBe('Already at the start.');
	media.position = 30;
	page.seeking();
	expect(page.read().jumpStatus).toBe('');
});

test.each([false, true])('a refused jump supersedes a pending share (during seeking: %s)', async (seeking) => {
	const { page, media, location, element } = restartFixture();
	media.duration = NaN;
	location.search = '?track=coral&t=45';
	page.applyShareLink();
	if (seeking) await Promise.resolve();
	expect(page.read().pendingShareSeek).not.toBeNull();
	media.duration = 120;
	media.refuse = true;
	if (seeking) {
		expect(media.position).toBe(45);
		vi.spyOn(element, 'currentTime', 'get').mockImplementationOnce(() => {
			page.jump('forward');
			return media.position;
		});
		page.seeking();
	} else page.jump('forward');
	expect(page.read().jumpStatus).toBe('The player did not accept that jump.');
	expect(page.read().pendingShareSeek).toBeNull();
	media.refuse = false;
	page.retryShare();
	expect(media.position).toBe(seeking ? 45 : 30);
});

test('a deferred Back cancels an older share before a metadata retry', async () => {
	const { page, element, media, location, writes } = restartFixture();
	let firstRead = true;
	Object.defineProperty(element, 'duration', { get() {
		if (firstRead) {
			firstRead = false;
			location.search = '?track=coral&t=45';
			page.applyShareLink();
			page.jump('back');
			element.dispatchEvent(new Event('loadedmetadata'));
		}
		return media.duration;
	} });
	page.jump('forward');
	await Promise.resolve();
	expect(writes).toEqual([20]);
	expect(media.position).toBe(20);
	expect(page.read().jumpStatus).toBe('Back 10 seconds, now at 0:20.');
});

test.each(['account', 'queue', 'attachment'] as const)(
	'jump cannot write after a native getter revokes its %s receipt', (boundary) => {
		const { page, element, writes, remount } = restartFixture();
		Object.defineProperty(element, 'seekable', { get() {
			if (boundary === 'account') page.resetAuth();
			if (boundary === 'queue') page.replaceQueue({ ...page.read().queue });
			if (boundary === 'attachment') remount();
			return { length: 1, start: () => 0, end: () => 120 };
		} });
		page.jump('forward');
		expect(writes).toEqual([]);
		expect(page.read().jumpStatus).toBe('');
	}
);

test('a newer share received by a jump source getter survives the old gesture', async () => {
	const { page, element, location, writes, track } = restartFixture();
	element.getAttribute = () => {
		element.getAttribute = () => track.streamUrl;
		location.search = '?track=coral&t=45';
		page.applyShareLink();
		return track.streamUrl;
	};
	page.jump('forward');
	expect(writes).toEqual([]);
	await Promise.resolve();
	expect(writes).toEqual([45]);
});

test.each([
	['jump', 1], ['jump', 2], ['restart', 2]
] as const)('%s cannot write if pre-write connectivity read %s changes the source', (gesture, order) => {
	const { page, element, media, writes } = restartFixture();
	let targetChecked = false;
	let connectivityReads = 0;
	let positionReads = 0;
	Object.defineProperties(element, {
		seekable: { get() {
			if (gesture === 'jump') targetChecked = true;
			return { length: 1, start: () => 0, end: () => 120 };
		} },
		currentTime: { get() {
			// Restart reads the position once for availability, then once for the write.
			if (gesture === 'restart' && ++positionReads === 2) targetChecked = true;
			return media.position;
		}, set(value: number) { writes.push(value); media.position = value; } },
		isConnected: { get() {
			if (targetChecked && ++connectivityReads === order) element.sourceAttribute = '/test-audio/replacement';
			return true;
		} }
	});
	if (gesture === 'restart') page.restart(); else page.jump('forward');
	expect(element.sourceAttribute).toBe('/test-audio/replacement');
	expect(writes).toEqual([]);
	expect(media.position).toBe(30);
	expect(page.read().jumpStatus).toBe('');
	expect(page.read().restartStatusVersion).toBe(0);
});

test('a source read that disconnects the element cannot authorize a jump', () => {
	const { page, element, track, writes } = restartFixture();
	let targetChecked = false;
	Object.defineProperty(element, 'seekable', { get() {
		targetChecked = true;
		return { length: 1, start: () => 0, end: () => 120 };
	} });
	element.getAttribute = () => {
		if (targetChecked) element.isConnected = false;
		return track.streamUrl;
	};
	page.jump('forward');
	expect(element.isConnected).toBe(false);
	expect(writes).toEqual([]);
	expect(page.read().jumpStatus).toBe('');
});

test.each(['before', 'after'])('unreadable source %s assignment retires feedback only if the jump wrote', (order) => {
	const { page, element, media, track, writes } = restartFixture();
	page.jump('forward');
	const priorStatus = 'Forward 10 seconds, now at 0:40.';
	expect(page.read().jumpStatus).toBe(priorStatus);
	let targetChecked = false;
	Object.defineProperty(element, 'seekable', { get() {
		targetChecked = true;
		return { length: 1, start: () => 0, end: () => 120 };
	} });
	element.getAttribute = () => {
		if (order === 'before' ? targetChecked : media.position === 50) throw new Error('unreadable source');
		return track.streamUrl;
	};
	page.jump('forward');
	expect(writes).toEqual(order === 'before' ? [40] : [40, 50]);
	expect(page.read().jumpStatus).toBe(order === 'before' ? priorStatus : '');
});

test.each(['before', 'after'])('a restart called %s the jump setter stores its value wins', (order) => {
	const { page, element, media, writes } = restartFixture();
	Object.defineProperty(element, 'currentTime', {
		get: () => media.position,
		set: (seconds: number) => {
			writes.push(seconds);
			if (seconds !== 0 && order === 'before') page.restart();
			media.position = seconds;
			page.seeking();
			if (seconds !== 0 && order === 'after') page.restart();
		}
	});
	page.jump('forward');
	expect(writes).toEqual([40, 0]);
	expect(media.position).toBe(0);
	expect(page.read().restartStatus).toBe('Restarted Coral Night.');
	expect(page.read().jumpStatus).toBe('');
});

test('a new share supersedes a restart deferred by the jump setter', async () => {
	const { page, element, media, location, writes } = restartFixture();
	Object.defineProperty(element, 'currentTime', { get: () => media.position, set(seconds: number) {
		writes.push(seconds);
		if (seconds === 40) {
			page.restart();
			location.search = '?track=coral&t=45';
			page.applyShareLink();
		}
		media.position = seconds;
	} });
	page.jump('forward');
	await Promise.resolve();
	expect(writes).toEqual([40, 45]);
	expect(media.position).toBe(45);
	expect(page.read().restartStatusVersion).toBe(0);
	expect(page.read().jumpStatus).toBe('');
});

test('restart clears an earlier jump position readout', () => {
	const { page, media } = restartFixture();
	page.jump('forward');
	expect(page.read().jumpStatus).toBe('Forward 10 seconds, now at 0:40.');
	page.restart();
	expect(media.position).toBe(0);
	expect(page.read().jumpStatus).toBe('');
});

test('a native source check that throws after assignment cannot preserve the prior jump readout', () => {
	const { page, media, element, writes, track } = restartFixture();
	page.jump('forward');
	expect(page.read().jumpStatus).toBe('Forward 10 seconds, now at 0:40.');
	element.getAttribute = () => {
		if (media.position === 0) throw new Error('unreadable source after assignment');
		return track.streamUrl;
	};
	page.restart();
	expect(writes).toEqual([40, 0]);
	expect(page.read().jumpStatus).toBe('');
	expect(page.read().restartStatusVersion).toBe(0);
});

test('unreadable restart readback reports failure without restoring the retired jump readout', () => {
	const { page, media, element, writes } = restartFixture();
	page.jump('forward');
	Object.defineProperty(element, 'currentTime', { get() {
		if (media.position === 0) throw new Error('unreadable position');
		return media.position;
	}, set(value: number) { writes.push(value); media.position = value; } });
	page.restart();
	expect(writes).toEqual([40, 0]);
	expect(page.read().restartStatus).toBe('Could not restart Coral Night.');
	expect(page.read().jumpStatus).toBe('');
});

test('restart at the start clears an earlier boundary readout', () => {
	const { page, media } = restartFixture();
	media.position = 0;
	page.jump('back');
	expect(page.read().jumpStatus).not.toBe('');
	page.restart();
	expect(media.position).toBe(0);
	expect(page.read().jumpStatus).toBe('');
});

for (const mode of ['one', 'queue'] as const) {
	test(`LIBREP-U03 native ended replays under repeat ${mode} and clears an earlier jump readout`, async () => {
		const { page, media, element, capture } = restartFixture();
		page.jump('forward');
		expect(page.read().jumpStatus).toBe('Forward 10 seconds, now at 0:40.');
		element.paused = false;
		capture('play')();
		media.position = media.duration;
		element.ended = element.paused = true;
		capture('pause')();
		element.play = async () => { element.ended = element.paused = false; };
		await page.end(mode);
		expect(media.position).toBe(0);
		expect(page.read().jumpStatus).toBe('');
		expect(page.read().queueAnnouncement).toBe('Playing Coral Night.');
	});
}

test.each(['tick', 'play', 'play rejection'] as const)('same-source repeat cannot cross a reattachment during %s', async (phase) => {
	let resume!: () => void;
	const pending = new Promise<void>((resolve, reject) => {
		resume = phase === 'play rejection' ? () => reject(new Error('native play refused')) : resolve;
	});
	const { page, element, media, capture, remount } = restartFixture(() => phase === 'tick' ? pending : Promise.resolve());
	element.play = vi.fn(() => pending);
	element.paused = false;
	capture('play')();
	media.position = media.duration;
	element.ended = element.paused = true;
	const repeat = page.end('one');
	await Promise.resolve();
	expect(element.play).toHaveBeenCalledTimes(phase === 'tick' ? 0 : 1);
	const replacement = remount();
	replacement.paused = false;
	capture('play')();
	// A successful stale play must be fenced by its receipt, not by native pause.
	if (phase === 'play') element.ended = element.paused = false;
	resume();
	await repeat;
	expect(media.position).toBe(phase === 'tick' ? media.duration : 0);
	expect(page.read().queueAnnouncement).toBe('');
	expect(page.nativeIntent().playing).toBe(true);
});

const pine = { id: 'pine', title: 'Pine Morning', artist: '', streamUrl: '/test-audio/pine' };

function signalNaturalEnd({ element, capture, media }: ReturnType<typeof restartFixture>): void {
	element.paused = false;
	capture('play')();
	media.position = media.duration;
	element.ended = element.paused = true;
	capture('pause')(); // The browser's completion pause is not a manual pause.
}

test('route selection drives native repeat, and cleanup retires only its own player', async () => {
	let flush!: () => void;
	const pending = new Promise<void>((resolve) => { flush = resolve; });
	const selected = restartFixture(() => pending);
	const leaving = restartFixture(() => pending);
	selected.page.replaceQueue(addTrack(selected.page.read().queue, pine));
	selected.page.select(1);
	const target = selected.remount(pine.streamUrl);
	target.play = vi.fn(async () => { target.ended = target.paused = false; });
	leaving.element.play = vi.fn(async () => { leaving.element.ended = leaving.element.paused = false; });

	signalNaturalEnd({ ...selected, element: target });
	signalNaturalEnd(leaving);
	const repeatSelection = selected.page.end('one');
	const retiredRepeat = leaving.page.end('one');
	leaving.page.cleanup();
	flush();
	await Promise.all([repeatSelection, retiredRepeat]);

	expect(currentTrack(selected.page.read().queue)).toBe(pine);
	expect(selected.page.currentAudio()).toBe(target);
	expect(target.play).toHaveBeenCalledOnce();
	expect(selected.writes).toEqual([0]);
	expect(selected.page.read().queueAnnouncement).toBe('Playing Pine Morning.');
	expect(currentTrack(leaving.page.read().queue)?.id).toBe(leaving.track.id);
	expect(leaving.page.currentAudio()).toBeUndefined();
	expect(leaving.element.play).not.toHaveBeenCalled();
	expect(leaving.writes).toEqual([]);
	expect(leaving.page.read().queueAnnouncement).toBe('');
});

test.each(['pause', 'auth', 'edit'] as const)('queue wrap is fenced by %s during the native flush', async (boundary) => {
	let flush!: () => void;
	const fixture = restartFixture(() => new Promise<void>((resolve) => { flush = resolve; }));
	const { page, track, remount, writes } = fixture;
	page.replaceQueue(selectTrack(createQueue([pine, { ...track, artist: '' }]), 1));
	signalNaturalEnd(fixture);
	const continuation = page.end('queue');
	expect(currentTrack(page.read().queue)?.id).toBe(pine.id);
	if (boundary === 'auth') page.resetAuth(); // While no attachment exists yet.
	const target = remount(pine.streamUrl);
	target.play = vi.fn(async () => {});
	if (boundary === 'pause') target.dispatchEvent(new Event('pause'));
	if (boundary === 'edit') page.replaceQueue(addTrack(page.read().queue, { ...track, id: 'oak', artist: '' }));
	flush();
	await continuation;
	expect(target.play).not.toHaveBeenCalled();
	expect(writes).toEqual([]);
	expect(page.read().queueAnnouncement).toBe('');
});

test.each(['plays', 'refuses', 'pauses before pause dispatch'] as const)('queue wrap transfers native settings and %s without a premature announcement', async (outcome) => {
	let settle!: () => void;
	const fixture = restartFixture();
	const { page, element, track, remount, writes } = fixture;
	page.replaceQueue(selectTrack(createQueue([pine, { ...track, artist: '' }]), 1));
	element.muted = true;
	signalNaturalEnd(fixture);
	const continuation = page.end('queue');
	const target = remount(pine.streamUrl);
	target.volume = 1;
	target.playbackRate = 1;
	target.play = vi.fn(() => new Promise<void>((resolve, reject) => {
		if (outcome !== 'refuses') target.paused = false;
		settle = outcome === 'refuses' ? () => reject(new Error('native play refused')) : resolve;
	}));
	await Promise.resolve();
	expect(target.play).toHaveBeenCalledTimes(1);
	expect(target).toMatchObject({ volume: 0.6, muted: true, playbackRate: 1.25 });
	expect(writes).toEqual([0]);
	expect(page.read().queueAnnouncement).toBe('');
	// Native pause changes the property immediately, but queues the pause event.
	if (outcome === 'pauses before pause dispatch') target.pause();
	settle();
	await continuation;
	expect(page.read().queueAnnouncement).toBe(outcome === 'plays' ? 'Playing Pine Morning.' : '');
	expect(page.nativeIntent().playing).toBe(outcome === 'plays');
});

test('end supersedes a deferred share adjustment but yields to a newer share during flush', async () => {
	const fixture = restartFixture();
	const { page, location, media, writes } = fixture;
	media.duration = NaN;
	location.search = '?track=coral&t=90';
	page.applyShareLink();
	await Promise.resolve();
	expect(page.read().pendingShareSeek).not.toBeNull();
	media.duration = 120;
	signalNaturalEnd(fixture);
	const continuation = page.end('one');
	expect(page.read().pendingShareSeek).toBeNull();
	page.retryShare();
	expect(writes).toEqual([90]); // No metadata retry of the original seek.
	location.search = '?track=coral&t=17';
	page.applyShareLink();
	await continuation;
	expect(writes).toEqual([90, 17]);
	expect(page.read().queueAnnouncement).toBe('');
});

test('source-preserving queue edits retain the current jump readout', () => {
	const { page, media } = restartFixture();
	page.jump('forward');
	page.replaceQueue(addTrack(page.read().queue, { id: 'pine', title: 'Pine', artist: '', streamUrl: '/test-audio/pine' }));
	expect(media.position).toBe(40);
	expect(page.read().jumpStatus).toBe('Forward 10 seconds, now at 0:40.');
});

for (const invalid of ['range', 'duration', 'position'] as const) {
	test(`restart refuses stale ${invalid} availability without writing currentTime`, () => {
		const { page, media, writes } = restartFixture();
		if (invalid === 'range') media.rangeStart = 10;
		if (invalid === 'duration') media.duration = Number.NaN;
		if (invalid === 'position') media.position = Number.NaN;
		// No media event is delivered between native-state change and gesture.
		page.restart();
		expect(writes).toEqual([]);
		expect(page.read().restartCanSeek).toBe(false);
		expect(page.read().restartStatus).toBe('Could not restart Coral Night.');
	});
}

for (const outcome of ['Restarted', 'Already at the start of', 'Could not restart'] as const) {
	test(`identical ${outcome} outcomes advance the live-region version once per gesture`, () => {
		const { page, media } = restartFixture();
		if (outcome === 'Already at the start of') media.position = 0;
		if (outcome === 'Could not restart') media.refuse = true;
		page.restart();
		const first = page.read();
		if (outcome === 'Restarted') media.position = 30;
		page.restart();
		const second = page.read();
		expect(first.restartStatus).toBe(`${outcome} Coral Night.`);
		expect(second.restartStatus).toBe(first.restartStatus);
		expect(second.restartStatusVersion).toBe(first.restartStatusVersion + 1);
	});
}

for (const outcome of ['Restarted', 'Already at the start of', 'Could not restart'] as const) {
	test(`${outcome} truncation never splits a surrogate pair`, () => {
		const { page, media, track } = restartFixture();
		if (outcome === 'Already at the start of') media.position = 0;
		if (outcome === 'Could not restart') media.refuse = true;
		// Place an astral character on the truncation boundary for every offset
		// so the cut lands between its surrogate halves.
		for (let lead = 70; lead < 100; lead += 1) {
			track.title = `${'A'.repeat(lead)}\u{1F3B5}${'B'.repeat(60)}`;
			media.position = outcome === 'Already at the start of' ? 0 : 30;
			page.restart();
			const status = page.read().restartStatus;
			expect(status.length).toBeLessThan(100);
			expect(status.isWellFormed()).toBe(true);
		}
	});
}
