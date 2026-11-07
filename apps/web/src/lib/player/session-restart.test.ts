import { expect, test, vi } from 'vitest';
import { createPlayerSession } from './session.js';
import { createQueue } from './queue.js';
import { RESUME_STORAGE_KEY } from './resume.js';

import { restartNative } from './nativeRestart.js';

for (const atStart of [false, true]) {
	for (const outcome of ['unavailable', 'throw', 'sticky', 'nan', 'infinity', 'negative-infinity', 'success'] as const) {
		test(`native reset boundary distinguishes ${outcome} ${atStart ? 'at start' : 'away from start'} without events`, () => {
			const audio = new AudioFake('/a');
			let position = atStart ? 0.02 : 30;
			let assigned = false;
			let reset = false;
			const trace: string[] = [];
			const writes: number[] = [];
			if (outcome === 'unavailable') audio.ranges = [];
			Object.defineProperty(audio, 'currentTime', {
				get: () => {
					if (assigned) expect(reset).toBe(true);
					trace.push('read');
					return position;
				},
				set: (value: number) => {
					writes.push(value);
					trace.push('setter');
					expect(reset).toBe(false);
					if (outcome === 'throw') throw new Error('refused');
					if (outcome === 'success') position = value;
					if (outcome === 'nan') position = NaN;
					if (outcome === 'infinity') position = Infinity;
					if (outcome === 'negative-infinity') position = -Infinity;
					assigned = true;
					trace.push('setter-return');
				}
			});
			const onAssigned = vi.fn(() => { trace.push('reset'); reset = true; });
			const result = restartNative(audio.element, 'Coral Night', { isCurrent: () => true, onAssigned });
			const normal = outcome !== 'unavailable' && outcome !== 'throw';
			expect(writes).toEqual(outcome === 'unavailable' ? [] : [0]);
			expect(onAssigned).toHaveBeenCalledTimes(normal ? 1 : 0);
			expect(reset).toBe(normal);
			if (normal) expect(trace.indexOf('reset')).toBe(trace.indexOf('setter-return') + 1);
			const words = outcome === 'unavailable' ? 'Could not restart'
				: atStart ? 'Already at the start of' : outcome === 'success' ? 'Restarted' : 'Could not restart';
			expect(result).toBe(`${words} Coral Night.`);
			for (const call of [audio.play, audio.pause, audio.load]) expect(call).not.toHaveBeenCalled();
		});
	}
}

for (const point of ['getter', 'setter', 'reset'] as const) {
	test(`native reset boundary fences reentry from the ${point}`, () => {
		const audio = new AudioFake('/a');
		let current = true;
		let position = 30;
		let reset = false;
		const writes: number[] = [];
		Object.defineProperty(audio, 'currentTime', {
			get: () => { if (point === 'getter') current = false; return position; },
			set: (value: number) => {
				writes.push(value);
				position = value;
				if (point === 'setter') current = false;
			}
		});
		const onAssigned = vi.fn(() => { reset = true; if (point === 'reset') current = false; });
		expect(restartNative(audio.element, 'Coral Night', { isCurrent: () => current, onAssigned })).toBe('Could not restart Coral Night.');
		expect(writes).toEqual(point === 'getter' ? [] : [0]);
		expect(reset).toBe(point === 'reset');
		expect(onAssigned).toHaveBeenCalledTimes(point === 'reset' ? 1 : 0);
	});
}

test('native reset callback faults are not classified as refused native assignments', () => {
	const audio = new AudioFake('/a');
	const failure = new Error('subscriber failure');
	const onAssigned = vi.fn(() => { throw failure; });
	expect(() => restartNative(audio.element, 'Coral Night', { isCurrent: () => true, onAssigned })).toThrow(failure);
	expect(audio.currentTime).toBe(0);
	expect(onAssigned).toHaveBeenCalledOnce();
});

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

for (const atStart of [false, true]) {
	for (const outcome of ['unavailable', 'throw', 'sticky', 'nan', 'infinity', 'negative-infinity', 'success'] as const) {
		test(`session reset retires jump at normal assignment: ${outcome}, atStart=${atStart}`, () => {
			const { session, audio } = setup();
			session.jumpRelative('forward');
			let position = atStart ? 0.02 : 40;
			if (atStart) {
				audio.currentTime = position;
				session.jumpRelative('back');
			}
			const before = session.snapshot();
			const writes: number[] = [];
			let assigned = false;
			Object.defineProperty(audio, 'currentTime', {
				get: () => {
					if (assigned) expect(session.snapshot().jumpStatus).toBe('');
					return position;
				},
				set: (value: number) => {
					writes.push(value);
					expect(session.snapshot().jumpStatus).toBe(before.jumpStatus);
					if (outcome === 'throw') throw new Error('refused');
					if (outcome === 'success') position = value;
					if (outcome === 'nan') position = NaN;
					if (outcome === 'infinity') position = Infinity;
					if (outcome === 'negative-infinity') position = -Infinity;
					assigned = true;
				}
			});
			if (outcome === 'unavailable') audio.ranges = [];
			const accepted = session.restart();
			const completed = outcome !== 'unavailable' && outcome !== 'throw';
			expect(writes).toEqual(outcome === 'unavailable' ? [] : [0]);
			expect(session.snapshot().jumpStatus).toBe(completed ? '' : before.jumpStatus);
			expect(session.snapshot().jumpStatusVersion).toBe(before.jumpStatusVersion);
			expect(session.snapshot().restartVersion).toBe(before.restartVersion + 1);
			const words = outcome === 'unavailable' ? 'Could not restart'
				: atStart ? 'Already at the start of' : outcome === 'success' ? 'Restarted' : 'Could not restart';
			expect(session.snapshot().restartStatus).toBe(`${words} Coral Night.`);
			expect(accepted).toBe(!words.startsWith('Could not'));
			expect(session.snapshot().queue).toBe(before.queue);
			for (const call of [audio.play, audio.pause, audio.load]) expect(call).not.toHaveBeenCalled();
			// No media event delivered the reset. An older queued seek cannot undo it.
			assigned = false;
			position = 40;
			audio.emit('seeking');
			expect(session.snapshot().jumpStatus).toBe(completed ? '' : before.jumpStatus);
			session.destroy();
		});
	}
}

for (const point of ['before-intent', 'getter', 'setter', 'reset-publication'] as const) {
	for (const boundary of ['source', 'url', 'auth', 'reattach', 'destroy', 'nested'] as const) {
		test(`position action fences ${boundary} reentry at ${point}`, () => {
			const { session, audio } = setup();
			session.jumpRelative('forward');
			const version = session.snapshot().jumpStatusVersion;
			let position = 40;
			let armed = true;
			const writes: number[] = [];
			const reenter = () => {
				if (!armed) return;
				armed = false;
				if (boundary === 'source') session.setQueue(createQueue([B]));
				if (boundary === 'url') session.setQueue(createQueue([{ ...A, streamUrl: '/new-a' }]));
				if (boundary === 'auth') session.resetAtAuthBoundary();
				if (boundary === 'destroy') session.destroy();
				if (boundary === 'reattach') session.attach(audio.element);
				if (boundary === 'nested') session.jumpRelative('back');
			};
			Object.defineProperty(audio, 'currentTime', {
				get: () => { if (point === 'getter') reenter(); return position; },
				set: (value: number) => { writes.push(value); position = value; if (point === 'setter') reenter(); }
			});
			if (point === 'before-intent') session.onBeforeRestart(reenter);
			const unsubscribe = session.subscribe((state) => {
				if (point === 'reset-publication' && state.jumpStatus === '') reenter();
			});
			const restartVersion = session.snapshot().restartVersion;
			expect(session.restart()).toBe(false);
			expect(session.snapshot().restartVersion).toBe(boundary === 'auth' ? 0 : restartVersion);
			if (boundary === 'nested') {
				expect(session.snapshot().jumpStatusVersion).toBe(version + 1);
				expect(session.snapshot().jumpStatus).not.toBe('');
			} else expect(session.snapshot().jumpStatus).toBe('');
			if (point === 'before-intent' || point === 'getter') expect(writes).not.toContain(0);
			else expect(writes.filter((value) => value === 0)).toHaveLength(1);
			unsubscribe();
			session.destroy();
		});
	}
}

for (const point of ['before-intent', 'getter', 'setter', 'publication'] as const) {
	for (const boundary of ['source', 'url', 'auth', 'reattach', 'destroy', 'nested'] as const) {
		test(`relative jump fences ${boundary} reentry at ${point}`, () => {
			const { session, audio } = setup();
			session.jumpRelative('forward');
			const before = session.snapshot();
			let armed = true;
			let position = 40;
			const writes: number[] = [];
			const reenter = () => {
				if (!armed) return;
				armed = false;
				if (boundary === 'source') session.setQueue(createQueue([B]));
				if (boundary === 'url') session.setQueue(createQueue([{ ...A, streamUrl: '/new-a' }]));
				if (boundary === 'auth') session.resetAtAuthBoundary();
				if (boundary === 'destroy') session.destroy();
				if (boundary === 'reattach') session.attach(audio.element);
				if (boundary === 'nested') session.jumpRelative('forward');
			};
			Object.defineProperty(audio, 'currentTime', {
				get: () => { if (point === 'getter') reenter(); return position; },
				set: (value: number) => { writes.push(value); position = value; if (point === 'setter') reenter(); }
			});
			if (point === 'before-intent') session.onBeforePositionIntent(reenter);
			const unsubscribe = session.subscribe((state) => {
				if (point === 'publication' && state.jumpStatusVersion === before.jumpStatusVersion + 1) reenter();
			});
			session.jumpRelative('back');
			if (boundary === 'nested') {
				expect(session.snapshot().jumpStatus).toMatch(/^Forward 10 seconds, now at /);
				expect(session.snapshot().jumpStatusVersion).toBe(before.jumpStatusVersion + (point === 'publication' ? 2 : 1));
			} else expect(session.snapshot().jumpStatus).toBe('');
			if (point === 'before-intent' || point === 'getter') expect(writes).not.toContain(30);
			else expect(writes.filter((value) => value === 30)).toHaveLength(1);
			expect(session.snapshot().restartVersion).toBe(before.restartVersion);
			unsubscribe();
			session.destroy();
		});
	}
}

test('old detach cannot retire the same element in a new attachment incarnation', () => {
	const { session, audio, detach } = setup();
	const nextDetach = session.attach(audio.element);
	session.jumpRelative('forward');
	const before = session.snapshot();
	detach();
	audio.emit('seeking');
	expect(session.snapshot().audio).toBe(audio.element);
	expect(session.snapshot().jumpStatus).toBe(before.jumpStatus);
	expect(session.snapshot().jumpStatusVersion).toBe(before.jumpStatusVersion);
	nextDetach();
	expect(session.snapshot().jumpStatus).toBe('');
	session.destroy();
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
