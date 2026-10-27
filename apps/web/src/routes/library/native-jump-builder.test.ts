import { expect, test } from 'vitest';
import ts from 'typescript';
import { addTrack, createQueue, currentTrack, removeTrack, selectTrack } from '#lib/player/queue.js';
import { describeJump, jumpMedia } from '#lib/player/relativeJump.js';

const sources = import.meta.glob('./+page.svelte', {
	query: '?raw', import: 'default', eager: true
}) as Record<string, string>;

// Run the real page handlers against a mutable media double. Rendering is
// covered by the browser bank; these pin the readout lifecycle it cannot.
function jumpFixture() {
	const source = sources['./+page.svelte'];
	const script = source.slice(source.indexOf('>') + 1, source.indexOf('</script'));
	const parsed = ts.createSourceFile('library.ts', script, ts.ScriptTarget.Latest, true);
	const names = new Set([
		'clearShareFeedback', 'replaceQueue', 'resetRestartOnSourceChange',
		'resetShareAtAuthBoundary', 'selectQueuedTrack', 'removeQueuedTrack', 'jumpRelative'
	]);
	const handlers = parsed.statements.filter((node) =>
		ts.isFunctionDeclaration(node) && node.name && names.has(node.name.text)
	).map((node) => node.getText(parsed)).join('\n');
	const compiled = ts.transpileModule(handlers, {
		compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ESNext }
	}).outputText;
	const coral = { id: 'coral', title: 'Coral Night', streamUrl: '/test-audio/coral' };
	const ember = { id: 'ember', title: 'Ember Road', streamUrl: '/test-audio/ember' };
	const media = { position: 20, duration: 120 };
	const element = {
		isConnected: true,
		get duration() { return media.duration; },
		get currentTime() { return media.position; },
		set currentTime(value: number) { media.position = value; },
		getAttribute: () => coral.streamUrl
	};
	const bindings = {
		queueCurrentTrack: currentTrack, selectQueueIndex: selectTrack, removeTrack,
		jumpMedia, describeJump, addTrack, createQueue, element, coral, ember
	};
	const page = new Function(...Object.keys(bindings), `
		let queue = addTrack(createQueue([coral]), ember), audioEl = element;
		let shareGeneration = 0, shareStatus = '', shareFallbackUrl = null, pendingShareSeek = null;
		let queueAnnouncement = '', queueAnnouncementVersion = 0;
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
