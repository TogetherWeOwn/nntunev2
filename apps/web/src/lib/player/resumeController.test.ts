import { afterEach, expect, test, vi } from 'vitest';
import { get } from 'svelte/store';
import { createMediaController } from './mediaController.js';
import { createQueue } from './queue.js';
import { createQueueController } from './queueController.js';
import { createResumeStore, RESUME_STORAGE_KEY } from './resume.js';
import { createResumeController } from './resumeController.js';

const accountA = 'a'.repeat(64);
const accountB = 'b'.repeat(64);
const accountKey = (scope: string) => `nntune:library-position:v1:${scope}`;
const coral = { id: 'coral', title: 'Coral', artist: '', streamUrl: '/coral.flac' };

class SyntheticAudio extends EventTarget {
	isConnected = true;
	source = coral.streamUrl;
	currentSrc = this.src;
	get src() { return new URL(this.source, 'https://library.example').href; }
	getAttribute(name: string) { return name === 'src' ? this.source : null; }
	get element() { return this as unknown as HTMLAudioElement; }

	currentTime = 0;
	duration = 120;
	seeking = false;
	ended = false;
	paused = true;
	error: MediaError | null = null;
	volume = 1;
	muted = false;
	playbackRate = 1;
	play = vi.fn(async () => { this.paused = false; });
	pause = vi.fn(() => { this.paused = true; });
}

function memoryStorage(initial: Record<string, string> = {}) {
	const positions = new Map(Object.entries(initial));
	return {
		positions,
		getItem: vi.fn((key: string) => positions.get(key) ?? null),
		setItem: vi.fn((key: string, value: string) => { positions.set(key, value); }),
		removeItem: vi.fn((key: string) => { positions.delete(key); })
	};
}

function setup(scope: string | null = accountA) {
	const controller = createResumeController();
	const storage = memoryStorage();
	const readStorage = vi.fn(() => storage);
	const native = new SyntheticAudio();
	let ownsAttachment = true;
	const mount = (audio = native, source = coral.streamUrl) => controller.attach({
		trackId: coral.id, audio: audio.element, source, ownsAttachment: () => ownsAttachment, storage: readStorage
	});
	controller.setScope(scope);
	return { controller, native, storage, readStorage, mount, retire: () => { ownsAttachment = false; } };
}

const emit = (audio: SyntheticAudio, type: string) => audio.dispatchEvent(new Event(type));
afterEach(() => vi.unstubAllGlobals());

test('saves only the current account and attachment, including same-source replacement', () => {
	const { controller, native, storage, mount } = setup();
	const first = mount()!;
	native.currentTime = 32;
	first.observe('sample');
	controller.setScope(accountA); // Same-account refresh retains the live receipt.
	native.currentTime = 34;
	first.observe('sample'); // Below the sample threshold.
	expect(storage.positions.get(accountKey(accountA))).toBe('{"coral":32}');
	first.observe('flush');

	const nextAudio = new SyntheticAudio();
	const next = mount(nextAudio)!;
	native.currentTime = 38;
	first.observe('seeked');
	first.detach();
	expect(storage.positions.get(accountKey(accountA))).toBe('{"coral":34}');
	nextAudio.currentTime = 17;
	next.detach();
	nextAudio.currentTime = 20;
	next.observe('flush');
	expect(storage.positions.get(accountKey(accountA))).toBe('{"coral":17}');

	const previousAccount = mount(nextAudio)!;
	controller.setScope(accountB);
	const accountBAudio = new SyntheticAudio();
	const accountBAttachment = mount(accountBAudio)!;
	previousAccount.detach();
	accountBAudio.currentTime = 6;
	accountBAttachment.observe('sample');
	expect(storage.positions).toEqual(new Map([
		[accountKey(accountA), '{"coral":17}'], [accountKey(accountB), '{"coral":6}']
	]));
});

test('refuses a stale selection, a changed src, and the prior decoder during a source change', () => {
	const { native, readStorage, mount, retire } = setup();
	const original = mount()!;
	native.currentTime = 32;
	native.source = '/replacement.flac';
	original.observe('flush'); // src attribute changed but the decoder still reports the old URL.
	const replacement = mount(native, native.source)!;
	replacement.observe('seeked'); // The old decoder is also wrong for the new attachment.
	native.currentSrc = native.src;
	retire();
	replacement.observe('seeked'); // Owner no longer selects this attachment.
	replacement.detach();
	expect(readStorage).not.toHaveBeenCalled();
});

test.each([null, 'malformed', 'a'.repeat(65)])('refuses scope %s before sign-out or a replacement body can settle', (scope) => {
	const { controller, native, readStorage, mount } = setup();
	const receipt = mount()!;
	native.currentTime = 15;
	controller.setScope(scope);
	receipt.observe('flush'); // Page hide before replacement account data arrives.
	expect(mount()).toBeUndefined();
	controller.setScope(accountA); // Signing back in cannot revive an old receipt.
	receipt.detach();
	expect(readStorage).not.toHaveBeenCalled();
});

test.each(['position', 'storage'] as const)('auth teardown during the %s read refuses the write', (phase) => {
	const { controller, native, storage, mount } = setup();
	const receipt = mount()!;
	native.currentTime = 21;
	const revoke = () => { controller.setScope(null); receipt.detach(); };
	if (phase === 'position') {
		Object.defineProperty(native, 'currentTime', { get() { revoke(); return 21; } });
	} else {
		storage.getItem.mockImplementationOnce(() => { revoke(); return null; });
	}
	receipt.observe('flush');
	expect(storage.setItem).not.toHaveBeenCalled();
});

test('a transient storage read failure preserves other positions and permits a later retry', () => {
	const { native, storage, mount } = setup();
	const receipt = mount()!;
	const key = accountKey(accountA);
	storage.positions.set(key, '{"kelp":15}');
	storage.getItem.mockImplementationOnce(() => { throw new Error('Storage unavailable'); });
	native.currentTime = 22;
	receipt.observe('flush');
	expect(storage.setItem).not.toHaveBeenCalled();
	receipt.observe('flush');
	expect(storage.positions.get(key)).toBe('{"kelp":15,"coral":22}');
});

test('observes a real seek to zero, refuses failed completion, and clears only the completed track', () => {
	const { native, storage, mount } = setup();
	const receipt = mount()!;
	const key = accountKey(accountA);
	storage.positions.set(key, '{"coral":32,"kelp":15}');
	receipt.observe('flush');
	expect(storage.getItem).not.toHaveBeenCalled();
	expect(native.currentTime).toBe(0);
	receipt.observe('seeked');
	expect(storage.positions.get(key)).toBe('{"coral":0,"kelp":15}');
	native.currentTime = native.duration;
	native.ended = true;
	native.error = { code: 3 } as MediaError;
	receipt.observe('ended');
	expect(storage.positions.get(key)).toBe('{"coral":0,"kelp":15}');
	native.error = null;
	receipt.observe('ended');
	native.ended = false;
	receipt.detach();
	expect(storage.positions.get(key)).toBe('{"kelp":15}');
});

test.each(['ended', 'flush'] as const)('completion can retry from %s after a storage failure', (retry) => {
	const { native, storage, mount } = setup();
	const receipt = mount()!;
	const key = accountKey(accountA);
	storage.positions.set(key, '{"coral":32,"kelp":15}');
	const failingOperation = retry === 'ended' ? storage.getItem : storage.setItem;
	failingOperation.mockImplementationOnce(() => { throw new Error('Storage unavailable'); });
	native.currentTime = native.duration;
	native.ended = true;
	receipt.observe('ended');
	expect(storage.positions.get(key)).toBe('{"coral":32,"kelp":15}');
	receipt.observe(retry);
	expect(storage.positions.get(key)).toBe('{"kelp":15}');
	storage.getItem.mockClear();
	receipt.detach();
	expect(storage.getItem).not.toHaveBeenCalled();
});

test('the existing Library host plays and resumes the same attachment with position persistence off', async () => {
	const native = new SyntheticAudio();
	const seeded = { [RESUME_STORAGE_KEY]: '{"coral":45}', [accountKey(accountA)]: '{"coral":51}' };
	const storage = memoryStorage(seeded);
	vi.stubGlobal('window', { localStorage: storage });
	const media = createMediaController(createQueueController(createQueue([coral])), undefined,
		(audio, source) => audio.isConnected && audio.getAttribute('src') === source);
	const detach = media.attach(native.element);
	try {
		expect(native.currentTime).toBe(0);
		await media.playCurrent();
		expect(get(media.playbackStatus)).toBe('playing');
		native.currentTime = 12;
		emit(native, 'timeupdate');
		media.pauseCurrent();
		emit(native, 'pause');
		expect(get(media.playbackStatus)).toBe('paused');
		await media.playCurrent();
		expect(get(media.playbackStatus)).toBe('playing');
		expect(native.play).toHaveBeenCalledTimes(2);
		expect(native.currentTime).toBe(12);
		expect(media.audio).toBe(native.element);
	} finally {
		detach();
		media.destroy();
	}
	expect(storage.getItem.mock.calls.flat().filter((key) => key.includes('resume') || key.startsWith('nntune:library-position:'))).toEqual([]);
	expect(storage.positions).toEqual(new Map(Object.entries(seeded)));
	expect(createResumeStore({ storage }).positionFor(coral.id)).toBe(45);
});
