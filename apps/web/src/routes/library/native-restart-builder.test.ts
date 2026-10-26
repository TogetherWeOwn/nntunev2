import { expect, test } from 'vitest';
import ts from 'typescript';
import { addTrack, createQueue, currentTrack, selectTrack } from '#lib/player/queue.js';
import { resolveStreamUrl } from '#lib/library/stream-url.js';
import { displayArtist, displayTitle } from '#lib/library/order.js';
import { resolveShareTarget } from '#lib/player/shareTimestamp.js';

const sources = import.meta.glob('./+page.svelte', {
	query: '?raw', import: 'default', eager: true
}) as Record<string, string>;

// Exercise the real page handlers with mutable native getters. Lifecycle and
// rendering are intentionally excluded here; the browser banks cover those.
function restartFixture() {
	const source = sources['./+page.svelte'];
	const script = source.slice(source.indexOf('>') + 1, source.indexOf('</script'));
	const parsed = ts.createSourceFile('library.ts', script, ts.ScriptTarget.Latest, true);
	const names = new Set([
		'selectTrack', 'clearShareFeedback', 'replaceQueue', 'resetRestartOnSourceChange',
		'applyShareLink', 'applyPendingShareSeek', 'seekableIncludesStart',
		'restartIsAvailable', 'refreshRestartAvailability', 'restartCurrentTrack', 'setRestartStatus'
	]);
	const handlers = parsed.statements.filter((node) =>
		ts.isFunctionDeclaration(node) && node.name && names.has(node.name.text)
	).map((node) => node.getText(parsed)).join('\n');
	const compiled = ts.transpileModule(handlers, {
		compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ESNext }
	}).outputText;
	const track = { id: 'coral', title: 'Coral Night', streamUrl: '/test-audio/coral' };
	const location = { search: '' };
	const writes: number[] = [];
	const media = { position: 30, duration: 120, rangeStart: 0, refuse: false };
	const element = {
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
		getAttribute: (name: string) => name === 'src' ? track.streamUrl : null
	};
	const bindings = {
		addTrack, createQueue, queueCurrentTrack: currentTrack, selectQueueIndex: selectTrack,
		resolveStreamUrl, displayArtist, displayTitle, resolveShareTarget,
		tick: () => Promise.resolve(), window: { location }, element, track
	};
	const page = new Function(...Object.keys(bindings), `
		let items = [track], queue = createQueue(items), audioEl = element;
		let loading = false, loadError = null, libraryUnavailable = false;
		let shareAlive = true, handledShareSearch = null, pendingShareSeek = null;
		let shareInvalid = false, shareGeneration = 0, shareStatus = '', shareFallbackUrl = null;
		let queueAnnouncement = '', restartCanSeek = false, restartStatus = '', restartStatusVersion = 0;
		const restartSupersededSources = new Set();
		${compiled}
		return {
			restart: restartCurrentTrack, applyShareLink, applyPendingShareSeek,
			refresh: () => refreshRestartAvailability({ currentTarget: element }),
			read: () => ({ restartCanSeek, restartStatus, restartStatusVersion, pendingShareSeek, queue })
		};
	`)(...Object.values(bindings)) as {
		restart: () => void;
		applyShareLink: () => void;
		applyPendingShareSeek: () => void;
		refresh: () => void;
		read: () => {
			restartCanSeek: boolean;
			restartStatus: string;
			restartStatusVersion: number;
			pendingShareSeek: unknown;
			queue: ReturnType<typeof createQueue>;
		};
	};
	page.refresh();
	expect(page.read().restartCanSeek).toBe(true);
	return { page, media, writes, location };
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
