import { expect, test } from 'vitest';
import ts from 'typescript';
import { addTrack, createQueue, currentTrack, nextTrack, removeTrack, selectTrack } from '#lib/player/queue.js';
import { describeJump, jumpMedia } from '#lib/player/relativeJump.js';

const sources = import.meta.glob('./+page.svelte', {
	query: '?raw', import: 'default', eager: true
}) as Record<string, string>;

// Run the real page handlers against a mutable media double. Mounted-page
// rendering and Next wiring are covered by library-relative-jump-builder.spec.ts.
function jumpFixture() {
	const source = sources['./+page.svelte'];
	const script = source.slice(source.indexOf('>') + 1, source.indexOf('</script'));
	const parsed = ts.createSourceFile('library.ts', script, ts.ScriptTarget.Latest, true);
	const names = new Set([
		'clearShareFeedback', 'replaceQueue', 'resetRestartOnSourceChange',
		'resetShareAtAuthBoundary', 'clearQueueShareFeedback', 'setQueueShareOutcome',
		'selectQueuedTrack', 'removeQueuedTrack', 'jumpRelative'
	]);
	const handlers = parsed.statements.filter((node) =>
		ts.isFunctionDeclaration(node) && node.name && names.has(node.name.text)
	).map((node) => node.getText(parsed)).join('\n');
	const compiled = ts.transpileModule(handlers, {
		compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ESNext }
	}).outputText;
	const coral = { id: 'coral', title: 'Coral Night', streamUrl: '/test-audio/coral' };
	const ember = { id: 'ember', title: 'Ember Road', streamUrl: '/test-audio/ember' };
	const media = {
		position: 20, duration: 120, isConnected: true, paused: true,
		pauseCalls: 0, playCalls: 0, writes: [] as number[]
	};
	const element = {
		get isConnected() { return media.isConnected; },
		get paused() { return media.paused; },
		pause() { media.pauseCalls += 1; media.paused = true; },
		play() { media.playCalls += 1; media.paused = false; return Promise.resolve(); },
		get duration() { return media.duration; },
		get currentTime() { return media.position; },
		set currentTime(value: number) { media.writes.push(value); media.position = value; },
		getAttribute: () => coral.streamUrl
	};
	const bindings = {
		queueCurrentTrack: currentTrack, selectQueueIndex: selectTrack, removeTrack,
		jumpMedia, describeJump, addTrack, createQueue, element, coral, ember
	};
	const page = new Function(...Object.keys(bindings), `
		let queue = addTrack(createQueue([coral]), ember), audioEl = element;
		let endGeneration = 0, playingElement = null;
		let shareGeneration = 0, shareStatus = '', shareFallbackUrl = null, pendingShareSeek = null;
		let queueAnnouncement = '', queueAnnouncementVersion = 0, playEpoch = 0;
		let queueExportAuthGeneration = 0, queueShareGeneration = 0, handledShareSearch = null;
		let queueShareOutcome = { status: '', fallbackUrl: null, forQueue: null };
		let shareInvalid = false;
		let restartCanSeek = false, restartStatus = '';
		const restartSupersededSources = new Set();
		let jumpFeedback = '', jumpStatusVersion = 0;
		const streamUrl = coral.streamUrl;
		${compiled}
		return {
			jump: jumpRelative, select: selectQueuedTrack, remove: removeQueuedTrack,
			replace: replaceQueue, authReset: resetShareAtAuthBoundary,
			read: () => ({ jumpFeedback, jumpStatusVersion, queue })
		};
	`)(...Object.values(bindings)) as {
		jump: (direction: 'back' | 'forward') => void;
		select: (index: number) => void;
		remove: (index: number) => void;
		replace: (next: ReturnType<typeof createQueue>) => void;
		authReset: () => void;
		read: () => {
			jumpFeedback: string;
			jumpStatusVersion: number;
			queue: ReturnType<typeof createQueue>;
		};
	};
	return { page, media };
}

test.each([
	{ direction: 'back' as const, target: 10, paused: false },
	{ direction: 'forward' as const, target: 30, paused: false },
	{ direction: 'back' as const, target: 10, paused: true },
	{ direction: 'forward' as const, target: 30, paused: true }
])('$direction preserves paused=$paused without pausing or restarting media', ({ direction, target, paused }) => {
	const { page, media } = jumpFixture();
	media.paused = paused;
	const queue = page.read().queue;
	page.jump(direction);
	expect(media.position).toBe(target);
	expect(media.writes).toEqual([target]);
	expect(media.pauseCalls).toBe(0);
	expect(media.playCalls).toBe(0);
	expect(media.paused).toBe(paused);
	expect(page.read().queue).toBe(queue);
	expect(page.read().jumpFeedback).toMatch(/10 seconds, now at/);
});

test.each(['back', 'forward'] as const)('%s refuses a detached matching-source element without writing', (direction) => {
	const { page, media } = jumpFixture();
	media.isConnected = false;
	page.jump(direction);
	expect(media.position).toBe(20);
	expect(media.writes).toEqual([]);
	expect(media.pauseCalls).toBe(0);
	expect(media.playCalls).toBe(0);
	expect(page.read().jumpFeedback).toBe('Cannot jump yet: this track has not reported its length.');
});

test('a jump readout is set, then cleared by Select of another row', () => {
	const { page } = jumpFixture();
	page.jump('forward');
	expect(page.read().jumpFeedback).toMatch(/^Forward 10 seconds/);
	page.select(1);
	expect(page.read().jumpFeedback).toBe('');
});

test('a jump readout is cleared when Remove advances the current source', () => {
	const { page } = jumpFixture();
	page.jump('forward');
	expect(page.read().jumpFeedback).not.toBe('');
	page.remove(0);
	expect(page.read().jumpFeedback).toBe('');
});

test('removing a non-current row keeps the readout for the unchanged source', () => {
	const { page } = jumpFixture();
	page.jump('forward');
	const before = page.read().jumpFeedback;
	page.remove(1);
	expect(page.read().jumpFeedback).toBe(before);
});

test('replacing the queue with Next clears the previous source readout', () => {
	const { page } = jumpFixture();
	page.jump('forward');
	expect(page.read().jumpFeedback).toMatch(/^Forward 10 seconds/);
	page.replace(nextTrack(page.read().queue));
	expect(currentTrack(page.read().queue)?.id).toBe('ember');
	expect(page.read().jumpFeedback).toBe('');
});

test('the auth boundary clears the jump readout', () => {
	const { page } = jumpFixture();
	page.jump('forward');
	expect(page.read().jumpFeedback).not.toBe('');
	page.authReset();
	expect(page.read().jumpFeedback).toBe('');
});

test('a repeated identical outcome advances the live-region version once per gesture', () => {
	const { page, media } = jumpFixture();
	media.position = 120;
	page.jump('forward');
	const first = page.read();
	page.jump('forward');
	const second = page.read();
	expect(second.jumpFeedback).toBe(first.jumpFeedback);
	expect(second.jumpStatusVersion).toBe(first.jumpStatusVersion + 1);
});
