import { expect, test } from 'vitest';
import ts from 'typescript';
import { addTrack, clearQueue, clearUpcoming, createQueue, currentTrack, keepCurrentAndUpcoming, moveTrack, naturalEndPolicy, removeTrack, selectTrack } from '#lib/player/queue.js';
import { resolveStreamUrl } from '#lib/library/stream-url.js';
import { displayArtist, displayTitle } from '#lib/library/order.js';
import { clearMiniPlayerHint, readMiniPlayerHint, setMiniPlayerHint } from '#lib/shell/mini-player.js';
import { resolveShareTarget } from '#lib/player/shareTimestamp.js';
import { createPendingShareSeek } from '#lib/player/pendingShareSeek.js';
import { createQueueController } from '#lib/player/queueController.js';
import { createMediaController } from '#lib/player/mediaController.js';
import { describeJump, jumpMedia } from '#lib/player/relativeJump.js';
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
		'applyShareLink', 'applyPendingShareSeek', 'handleSeeking', 'selectQueuedTrack', 'removeQueuedTrack',
		'moveQueuedTrack', 'clearQueuedTracks', 'trimPreviousTracks', 'trimUpcomingTracks',
		'refreshRestartAvailability', 'restartCurrentTrack', 'setRestartStatus', 'jumpRelative',
		'attachNativeAudio', 'handleEnded', 'shortTitle', 'playQueuedTrack', 'playSelectedTrack'
	]);
	const declarations = parsed.statements.filter(ts.isFunctionDeclaration).filter((node) =>
		node.name && names.has(node.name.text)
	);
	expect(new Set(declarations.map((node) => node.name?.text))).toEqual(names);
	const seekState = parsed.statements.find((node) => ts.isVariableStatement(node) &&
		node.declarationList.declarations.some((item) => item.name.getText(parsed) === 'pendingShareSeek'));
	if (!seekState) throw new Error('Missing pending share transaction');
	const mount = parsed.statements.find((node) => ts.isExpressionStatement(node) &&
		ts.isCallExpression(node.expression) && node.expression.expression.getText(parsed) === 'onMount');
	if (!mount) throw new Error('Missing route mount');
	const handlers = [seekState, ...declarations, mount].map((node) => node.getText(parsed)).join('\n');
	const compiled = ts.transpileModule(handlers, {
		compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ESNext }
	}).outputText;
	const track = { id: 'coral', title: 'Coral Night', streamUrl: '/test-audio/coral' };
	const location = { search: '' };
	const writes: number[] = [];
	const media = { position: 30, duration: 120, rangeStart: 0, refuse: false };
	const listeners = new Map<string, (event: Event) => unknown>();
	const element = {
		addEventListener: (type: string, listener: (event: Event) => unknown) => { listeners.set(type, listener); },
		get duration() { return media.duration; },
		get currentTime() { return media.position; },
		set currentTime(value: number) {
			writes.push(value);
			if (media.refuse) throw new Error('native seek refused');
			media.position = value;
		},
		get seekable() {
			return { length: 1, start: () => media.rangeStart, end: () => media.duration };
		},
		getAttribute: (name: string) => name === 'src' ? track.streamUrl : null,
		isConnected: true, ended: false, paused: true, error: null as Pick<MediaError, 'code' | 'message'> | null,
		volume: 0.6, muted: false, playbackRate: 1.25,
		play: () => Promise.resolve(), pause: () => { element.paused = true; }
	};
	const capture = (type: string) => {
		const listener = listeners.get(type);
		if (!listener) throw new Error(`Missing native listener: ${type}`);
		return (currentTarget: unknown = element) => listener({ type, currentTarget } as Event);
	};
	let onClear = () => {};
	const trackedPendingShareSeek: typeof createPendingShareSeek = (...args) => {
		const transaction = createPendingShareSeek(...args);
		const clear = transaction.clear;
		transaction.clear = () => { onClear(); clear(); };
		return transaction;
	};
	const bindings = {
		addTrack, clearQueue, clearUpcoming, createQueue, keepCurrentAndUpcoming, moveTrack,
		queueCurrentTrack: currentTrack, selectQueueIndex: selectTrack, removeTrack,
		resolveStreamUrl, displayArtist, displayTitle, resolveShareTarget, describeJump, jumpMedia,
		clearMiniPlayerHint, setMiniPlayerHint, naturalEndPolicy, restartIsAvailable, createPendingShareSeek: trackedPendingShareSeek,
		createQueueController, createMediaController, streamUrl: track.streamUrl,
		tick, window: { location }, element, track, capture
	};
	const page = new Function(...Object.keys(bindings), `
		let items = [track], queue = createQueue(items), mediaRevision = 0;
		const media = createMediaController();
		let loading = false, loadError = null, libraryUnavailable = false;
		let shareAlive = true, handledShareSearch = null;
		let shareInvalid = false, shareGeneration = 0, shareStatus = '', shareFallbackUrl = null;
		let queueConfirmGeneration = 0;
		let queueAnnouncement = '', restartCanSeek = false, restartStatus = '', restartStatusVersion = 0;
		let jumpStatus = '', jumpStatusVersion = 0, lastJumpSeek = null;
		const restartSupersededSources = new Set();
		const queueController = createQueueController(queue);
		const library = queueController.prepareLibrary(items);
		queueController.publish(library.next, queueController.begin(library.next, 'replace', true), library.sources);
		queueController.subscribe((value) => { queue = value; });
		const queueRevision = queueController;
		let endGeneration = 0, queueAnnouncementVersion = 0;
		let repeat = 'off';
		let density, priorDocumentTitle = null, tabTitle = '';
		let cleanup;
		const onMount = (callback) => { cleanup = callback(); };
		const readStoredDensity = () => 'comfortable', readStoredRepeat = () => 'off';
		const handleAuthTransition = () => {}, recordFindQuery = () => {};
		const subscribeLocalAuthTransition = () => () => {}, handleLocalAuthTransition = () => {};
		const clearSnapshot = () => {}, browserStorage = () => null, refresh = () => {};
		const endTrackDrag = () => {}, document = { title: '' };
		const queueShare = { dispose() {} }, favoritesExport = { destroy() {} }, refresher = { dispose() {} };
		window.addEventListener = () => {}; window.removeEventListener = () => {};
		${compiled}
		attachNativeAudio(element);
		return {
			restart: restartCurrentTrack, applyShareLink, applyPendingShareSeek, jump: jumpRelative,
			select: selectQueuedTrack, remove: removeQueuedTrack, playQueued: playQueuedTrack,
			move: moveQueuedTrack, clear: clearQueuedTracks, cleanup,
			trimPrevious: trimPreviousTracks, trimUpcoming: trimUpcomingTracks,
			seeking: (target = element) => capture('seeking')(target), replaceQueue,
			subscribe: queueController.subscribe, attach: () => attachNativeAudio(element), currentAudio: () => media.audio,
			nativeIntent: () => ({ playing: media.attachment?.playing, endGeneration }),
			end: (mode) => { repeat = mode; return capture('ended')(); },
			refresh: () => capture('timeupdate')(),
			read: () => ({ restartCanSeek, restartStatus, restartStatusVersion, pendingShareSeek: pendingShareSeek.current, queue, jumpStatus, playEpoch: queueRevision.revision, mediaRevision, queueAnnouncement, queueAnnouncementVersion })
		};
	`)(...Object.values(bindings)) as {
		restart: () => void;
		applyShareLink: () => void;
		applyPendingShareSeek: () => void;
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
		attach: () => () => void;
		currentAudio: () => unknown;
		nativeIntent: () => { playing: boolean | undefined; endGeneration: number };
		subscribe: (callback: (queue: ReturnType<typeof createQueue>) => void) => () => void;
		end: (mode: 'one' | 'queue') => Promise<void>;
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
	return { page, media, writes, location, track, element, capture, observeClear: (callback: () => void) => { onClear = callback; } };
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
	const { page, media, location, writes } = restartFixture();
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
	page.attach();
	media.position = 12;
	media.duration = 20;
	page.applyPendingShareSeek();
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
	const { page, element } = restartFixture();
	const original = page.read().queue;
	const next = createQueue(original.tracks.map((track) => ({ ...track, streamUrl: '/test-audio/new' })));
	const staleCleanup = page.attach();
	const observed: unknown[] = [];
	const unsubscribe = page.subscribe(() => observed.push(page.currentAudio()));
	page.replaceQueue(next);
	page.replaceQueue(original);
	expect(observed).toEqual([element, undefined, undefined]);
	expect(page.read().mediaRevision).toBe(2);
	page.attach();
	staleCleanup();
	expect(page.currentAudio()).toBe(element);
	unsubscribe();
});

test('old native listeners cannot change intent or position feedback on the same node and source', () => {
	const { page, element, media, capture } = restartFixture();
	const staleCleanup = page.attach();
	const [play, pause, error, ended, seeking, progress] =
		['play', 'pause', 'error', 'ended', 'seeking', 'progress'].map(capture);
	page.attach();
	staleCleanup();
	element.paused = false;
	play();
	expect(page.nativeIntent().playing).toBe(false);
	capture('playing')();
	page.jump('forward');
	const before = page.nativeIntent();
	expect(before.playing).toBe(true);
	element.paused = true;
	pause();
	element.error = { code: 3, message: 'decode failed' };
	error();
	element.error = null;
	element.ended = true;
	ended();
	media.position = 80;
	media.duration = NaN;
	seeking();
	progress();
	expect(page.nativeIntent()).toEqual(before);
	expect(page.read().jumpStatus).toBe('Forward 10 seconds, now at 0:40.');
	expect(page.read().restartCanSeek).toBe(true);
	element.ended = false;
	capture('pause')();
	expect(page.nativeIntent()).toEqual({ playing: false, endGeneration: before.endGeneration + 1 });
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
	const { page, element } = restartFixture();
	const pine = { id: 'pine', title: 'Pine', artist: '', streamUrl: '/test-audio/pine' };
	page.replaceQueue({ tracks: [pine, { id: 'coral', title: 'Coral Night', artist: '', streamUrl: '' }], currentIndex: 0 });
	let plays = 0, deliveries = 0, replaced = 0;
	element.play = () => { plays += 1; return Promise.resolve(); };
	const unsubscribe = page.subscribe((value) => {
		deliveries += 1;
		if (value.currentIndex === 1) { replaced += 1; page.replaceQueue(value); }
	});
	const attempt = page.playQueued('coral');
	page.attach();
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

test('a refused jump still supersedes a pending share seek for the source', () => {
	const { page, media, location } = restartFixture();
	media.duration = Number.NaN;
	location.search = '?track=coral&t=45';
	page.applyShareLink();
	expect(page.read().pendingShareSeek).not.toBeNull();
	media.duration = 120;
	media.refuse = true;
	page.jump('forward');
	expect(page.read().jumpStatus).toBe('The player did not accept that jump.');
	expect(page.read().pendingShareSeek).toBeNull();
	media.refuse = false;
	page.applyPendingShareSeek();
	expect(media.position).toBe(30);
});

test('restart clears an earlier jump position readout', () => {
	const { page, media } = restartFixture();
	page.jump('forward');
	expect(page.read().jumpStatus).toBe('Forward 10 seconds, now at 0:40.');
	page.restart();
	expect(media.position).toBe(0);
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
	test(`same-source repeat ${mode} clears an earlier jump readout`, async () => {
		const { page, media, element, capture } = restartFixture();
		page.jump('forward');
		expect(page.read().jumpStatus).toBe('Forward 10 seconds, now at 0:40.');
		element.paused = false;
		capture('play')();
		media.position = media.duration;
		element.ended = element.paused = true;
		capture('pause')();
		await page.end(mode);
		expect(media.position).toBe(0);
		expect(page.read().jumpStatus).toBe('');
	});
}

test.each(['tick', 'play'] as const)('same-source repeat cannot cross a reattachment during %s', async (phase) => {
	let resume!: () => void;
	const pending = new Promise<void>((resolve) => { resume = resolve; });
	const { page, element, media, capture } = restartFixture(() => phase === 'tick' ? pending : Promise.resolve());
	element.play = () => pending;
	element.paused = false;
	capture('play')();
	media.position = media.duration;
	element.ended = element.paused = true;
	const repeat = page.end('one');
	await Promise.resolve();
	page.attach();
	element.ended = element.paused = false;
	capture('play')();
	resume();
	await repeat;
	expect(media.position).toBe(phase === 'tick' ? media.duration : 0);
	expect(page.read().queueAnnouncement).toBe('');
	expect(page.nativeIntent().playing).toBe(true);
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
