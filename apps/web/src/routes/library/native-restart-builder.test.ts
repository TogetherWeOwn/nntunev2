import { expect, test } from 'vitest';
import ts from 'typescript';
import { createPlayerSession } from '#lib/player/session.js';
import { queueViewActions } from '#lib/player/queueViewActions.js';
import { addTrack, createQueue, currentTrack, selectTrack } from '#lib/player/queue.js';
import { resolveStreamUrl } from '#lib/library/stream-url.js';
import { displayArtist, displayTitle } from '#lib/library/order.js';
import { clearMiniPlayerHint, setMiniPlayerHint } from '#lib/shell/mini-player.js';
import { resolveShareTarget } from '#lib/player/shareTimestamp.js';

const sources = import.meta.glob('./+page.svelte', {
	query: '?raw', import: 'default', eager: true
}) as Record<string, string>;

// Exercise the real persistent session (restart, relative jump, queue edits,
// natural end) and the route's own share handlers with native getters and
// events on one attached element. Component lifecycle and rendering are
// intentionally excluded here; the browser banks cover those.
function restartFixture() {
	const source = sources['./+page.svelte'];
	const script = source.slice(source.indexOf('>') + 1, source.indexOf('</script'));
	const parsed = ts.createSourceFile('library.ts', script, ts.ScriptTarget.Latest, true);
	const names = new Set([
		'queuedTrackFor', 'playableTrackFor', 'selectTrack', 'clearShareFeedback', 'replaceQueue', 'retireQueueWork', 'sourceProjections', 'libraryInputCurrent', 'syncMiniPlayerHint',
		'applyShareLink', 'applyPendingShareSeek', 'supersedePositionSeek', 'supersedeShareSeek'
	]);
	const handlers = parsed.statements.filter((node) =>
		ts.isFunctionDeclaration(node) && node.name && names.has(node.name.text)
	).map((node) => node.getText(parsed)).join('\n');
	const compiled = ts.transpileModule(handlers, {
		compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ESNext }
	}).outputText;
	const track = { id: 'coral', title: 'Coral Night', streamUrl: '/test-audio/coral' };
	// The element's source is its attachment, not whatever row the queue selects.
	const attachedSrc = track.streamUrl;
	const location = { search: '' };
	const writes: number[] = [];
	const media = { position: 30, duration: 120, rangeStart: 0, refuse: false };
	const element = Object.assign(new EventTarget(), {
		paused: true, ended: false, error: null, readyState: 4, isConnected: true,
		volume: 0.6, muted: false, playbackRate: 1.25, pause() {}, play: () => Promise.resolve(), load() {},
	});
	Object.defineProperties(element, {
		duration: { get: () => media.duration },
		currentTime: {
			get: () => media.position,
			set: (value: number) => {
				writes.push(value);
				if (media.refuse) throw new Error('native seek refused');
				media.position = value;
			}
		},
		seekable: { get: () => ({ length: 1, start: () => media.rangeStart, end: () => media.duration }) },
		getAttribute: { value: (name: string) => name === 'src' ? attachedSrc : null }
	});
	const bindings = {
		addTrack, createQueue, queueCurrentTrack: currentTrack, selectQueueIndex: selectTrack,
		resolveStreamUrl, displayArtist, displayTitle, resolveShareTarget,
		clearMiniPlayerHint, setMiniPlayerHint, createPlayerSession, queueViewActions,
		tick: () => Promise.resolve(), window: { location }, element, track, media
	};
	const page = new Function(...Object.keys(bindings), `
		let items = [track], queue = createQueue(items), audioEl = element;
		const player = createPlayerSession({ flush: () => Promise.resolve() });
		player.setQueue(queue);
		player.start(null);
		const detach = player.attach(element);
		const unsubscribe = player.subscribe((s) => { queue = s.queue; audioEl = s.audio ?? undefined; });
		let loading = false, loadError = null, libraryUnavailable = false;
		let shareAlive = true, handledShareSearch = null, pendingShareSeek = null;
		let shareInvalid = false, shareGeneration = 0, shareStatus = '', shareFallbackUrl = null;
		let queueConfirmGeneration = 0, libraryInputOwner = player.sourceInputOwner();
		let queueAnnouncement = '';
		const restartSupersededSources = new Set();
		let playEpoch = 0;
		${compiled}
		const releaseRestart = player.onBeforeRestart(supersedeShareSeek);
		const releaseIntent = player.onBeforePositionIntent(supersedePositionSeek);
		const queueActions = queueViewActions(player);
		const settle = () => new Promise((resolve) => setTimeout(resolve, 0));
		return {
			restart: player.restart, applyShareLink, applyPendingShareSeek,
			jump: player.jumpRelative,
			select: queueActions.onselect, remove: queueActions.onremove,
			seeking: (target) => {
				// Only the attached element is observed; any other target is a distinct source.
				const origin = target === undefined || target === element ? element : new EventTarget();
				origin.dispatchEvent(new Event('seeking'));
			},
			replaceQueue,
			end: async (mode) => {
				player.setRepeat(mode);
				// A genuine natural end: the element plays, its clock advances to the
				// duration without a terminal seek, then it reports ended.
				element.paused = false;
				element.ended = false;
				element.dispatchEvent(new Event('play'));
				element.dispatchEvent(new Event('playing'));
				const duration = media.duration;
				if (Number.isFinite(duration) && media.position < duration) {
					const step = (duration - media.position) / 4;
					for (let i = 1; i <= 4; i += 1) {
						media.position = i === 4 ? duration : media.position + step;
						element.dispatchEvent(new Event('timeupdate'));
					}
				} else element.dispatchEvent(new Event('timeupdate'));
				element.ended = true;
				element.dispatchEvent(new Event('ended'));
				await settle();
			},
			refresh: () => element.dispatchEvent(new Event('progress')),
			dispose: () => { releaseIntent(); releaseRestart(); unsubscribe(); detach(); player.destroy(); },
			read: () => ({
				restartCanSeek: player.snapshot().restartAvailable,
				restartStatus: player.snapshot().restartStatus,
				restartStatusVersion: player.snapshot().restartVersion,
				pendingShareSeek, queue: player.snapshot().queue,
				jumpStatus: player.snapshot().jumpStatus
			})
		};
	`)(...Object.values(bindings)) as {
		restart: () => void;
		applyShareLink: () => void;
		applyPendingShareSeek: () => void;
		jump: (direction: 'back' | 'forward') => void;
		select: (index: number) => void;
		remove: (index: number) => void;
		seeking: (target?: unknown) => void;
		replaceQueue: (queue: ReturnType<typeof createQueue>) => void;
		end: (mode: 'one' | 'queue') => Promise<void>;
		refresh: () => void;
		dispose: () => void;
		read: () => {
			restartCanSeek: boolean;
			restartStatus: string;
			restartStatusVersion: number;
			pendingShareSeek: unknown;
			queue: ReturnType<typeof createQueue>;
			jumpStatus: string;
		};
	};
	page.refresh();
	expect(page.read().restartCanSeek).toBe(true);
	return { page, media, writes, location, track };
}

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
		const { page, media } = restartFixture();
		page.jump('forward');
		expect(page.read().jumpStatus).toBe('Forward 10 seconds, now at 0:40.');
		media.position = media.duration;
		await page.end(mode);
		expect(media.position).toBe(0);
		expect(page.read().jumpStatus).toBe('');
	});
}

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
