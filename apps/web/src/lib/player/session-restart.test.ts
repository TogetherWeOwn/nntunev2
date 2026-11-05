import { expect, test, vi } from 'vitest';
import { createPlayerSession } from './session.js';
import { createQueue } from './queue.js';
import { RESUME_STORAGE_KEY } from './resume.js';

const A = { id: 'a', title: 'Coral Night', artist: 'Mira', streamUrl: '/a' };
const B = { id: 'b', title: 'Pine Morning', artist: 'North', streamUrl: '/b' };

class AudioFake extends EventTarget {
	isConnected = true;
	paused = true;
	ended = false;
	error = null;
	currentTime = 30;
	duration = 120;
	volume = 1;
	muted = false;
	playbackRate = 1;
	readyState = 4;
	ranges: [number, number][] = [[0, 120]];
	constructor(public src: string) { super(); }
	getAttribute(name: string) { return name === 'src' ? this.src : null; }
	get seekable() {
		return { length: this.ranges.length, start: (i: number) => this.ranges[i][0], end: (i: number) => this.ranges[i][1] };
	}
	play = vi.fn(async () => {});
	pause = vi.fn(() => {});
	load = vi.fn();
	emit(name: string) { this.dispatchEvent(new Event(name)); }
	get element() { return this as unknown as HTMLAudioElement; }
}

function setup() {
	const session = createPlayerSession({ flush: async () => {} });
	session.setQueue(createQueue([A, B]));
	session.start(null);
	const audio = new AudioFake('/a');
	const detach = session.attach(audio.element);
	return { session, audio, detach };
}

for (const paused of [true, false]) {
	test(`restart preserves ${paused ? 'paused' : 'playing'} intent, source, queue, rate and volume without transport calls`, () => {
		const { session, audio } = setup();
		audio.paused = paused;
		audio.playbackRate = 1.5;
		audio.volume = 0.4;
		const before = session.snapshot().queue;
		expect(session.restart()).toBe(true);
		expect(audio.currentTime).toBe(0);
		expect(audio.paused).toBe(paused);
		expect(audio.src).toBe('/a');
		expect(audio.playbackRate).toBe(1.5);
		expect(audio.volume).toBe(0.4);
		expect(session.snapshot().queue).toBe(before);
		expect(session.snapshot()).toMatchObject({ restartStatus: 'Restarted Coral Night.', restartVersion: 1 });
		for (const call of [audio.play, audio.pause, audio.load]) expect(call).not.toHaveBeenCalled();
	});
}

for (const invalid of ['duration', 'position', 'range', 'missing-range'] as const) {
	test(`restart refuses live ${invalid} changes without requiring a preceding event`, () => {
		const { session, audio } = setup();
		expect(session.snapshot().restartAvailable).toBe(true);
		if (invalid === 'duration') audio.duration = NaN;
		if (invalid === 'position') audio.currentTime = NaN;
		if (invalid === 'range') audio.ranges = [[10, 120]];
		if (invalid === 'missing-range') audio.ranges = [];
		const position = audio.currentTime;
		expect(session.restart()).toBe(false);
		expect(audio.currentTime).toBe(position);
		expect(session.snapshot()).toMatchObject({ restartAvailable: false, restartStatus: 'Could not restart Coral Night.' });
		audio.currentTime = 30;
		audio.duration = 120;
		audio.ranges = [[0, 120]];
		audio.emit('progress');
		expect(session.snapshot().restartAvailable).toBe(true);
		expect(session.restart()).toBe(true);
	});
}

for (const invalid of ['source', 'detached'] as const) {
	test(`restart ignores a ${invalid} audio owner and never emits an old success`, () => {
		const { session, audio } = setup();
		if (invalid === 'source') audio.src = '/stale';
		else audio.isConnected = false;
		audio.emit('timeupdate');
		expect(session.snapshot().restartAvailable).toBe(false);
		expect(session.restart()).toBe(false);
		expect(audio.currentTime).toBe(30);
		expect(session.snapshot().restartStatus).toBe('');
	});
}

for (const setter of ['throws', 'sticky'] as const) {
	test(`restart reports ${setter} native setter refusal from readback`, () => {
		const { session, audio } = setup();
		Object.defineProperty(audio, 'currentTime', { get: () => 30, set: () => { if (setter === 'throws') throw new Error('refused'); } });
		expect(session.restart()).toBe(false);
		expect(audio.currentTime).toBe(30);
		expect(session.snapshot().restartStatus).toBe('Could not restart Coral Night.');
	});
}

test('source replacement retires detached availability events, restart outcome and cleanup', () => {
	const { session, audio, detach } = setup();
	session.restart();
	session.setQueue(createQueue([B]));
	const next = new AudioFake('/b');
	session.attach(next.element);
	for (const event of ['loadedmetadata', 'durationchange', 'progress', 'timeupdate']) audio.emit(event);
	detach();
	expect(session.snapshot().audio).toBe(next.element);
	expect(session.snapshot()).toMatchObject({ restartAvailable: true, restartStatus: '' });
	expect(session.restart()).toBe(true);
	expect(next.currentTime).toBe(0);
	expect(audio.currentTime).toBe(0);
	expect(session.snapshot().restartStatus).toBe('Restarted Pine Morning.');
});

test('auth teardown during a before-restart notification prevents stale writes and outcomes', () => {
	const { session, audio } = setup();
	session.onBeforeRestart(() => session.resetAtAuthBoundary());
	expect(session.restart()).toBe(false);
	expect(audio.currentTime).toBe(30);
	expect(session.snapshot()).toMatchObject({ audio: null, restartAvailable: false, restartStatus: '', restartVersion: 0 });
});

test('unsubscribing the share seam removes its callback without disabling persistent restart', () => {
	const { session } = setup();
	const callback = vi.fn();
	const off = session.onBeforeRestart(callback);
	session.restart();
	expect(callback).toHaveBeenCalledExactlyOnceWith('a', '/a');
	off();
	session.restart();
	expect(callback).toHaveBeenCalledTimes(1);
	expect(session.snapshot().restartStatus).toBe('Already at the start of Coral Night.');
});

test('even refused restart supersedes deferred resume before late native metadata', () => {
	const session = createPlayerSession({ flush: async () => {} });
	session.setQueue(createQueue([A]));
	const storage = new Map([[RESUME_STORAGE_KEY, '{"a":77}']]);
	session.start({ getItem: (key) => storage.get(key) ?? null, setItem: (key, value) => { storage.set(key, value); }, removeItem: (key) => { storage.delete(key); } });
	const audio = new AudioFake('/a');
	audio.duration = NaN;
	session.attach(audio.element);
	expect(session.restart()).toBe(false);
	audio.duration = 120;
	audio.emit('loadedmetadata');
	expect(audio.currentTime).toBe(30);
});

for (const raw of ['', ' \t ', ' /a ']) {
	test(`session normalizes queued stream input ${JSON.stringify(raw)} before attachment`, () => {
		const session = createPlayerSession({ flush: async () => {} });
		session.setQueue(createQueue([{ ...A, streamUrl: raw }]));
		expect(session.snapshot().queue.tracks[0].streamUrl).toBe(raw.trim());
		session.start(null);
		const audio = new AudioFake(raw);
		session.attach(audio.element);
		expect(session.snapshot().audio).toBeNull();
		if (raw.trim()) {
			audio.src = raw.trim();
			session.attach(audio.element);
			expect(session.snapshot().audio).toBe(audio.element);
		}
	});
}
