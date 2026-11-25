import { describe, expect, it, vi } from 'vitest';
import { createBufferScheduler } from './bufferScheduler.js';

function syntheticBuffer(seconds: number): AudioBuffer {
	const sampleRate = 8_000;
	const samples = new Float32Array(seconds * sampleRate);
	return {
		sampleRate,
		length: samples.length,
		duration: samples.length / sampleRate,
		numberOfChannels: 1,
		getChannelData: () => samples
	} as AudioBuffer;
}

function fakeSource() {
	return {
		buffer: null as AudioBuffer | null,
		onended: null as (() => void) | null,
		start: vi.fn<(time: number) => void>(),
		stop: vi.fn(),
		connect: vi.fn(),
		disconnect: vi.fn()
	};
}

function fakeGain() {
	return {
		gain: { setValueAtTime: vi.fn(), linearRampToValueAtTime: vi.fn() },
		connect: vi.fn(),
		disconnect: vi.fn()
	};
}

function setup() {
	const sources: ReturnType<typeof fakeSource>[] = [];
	const gains: ReturnType<typeof fakeGain>[] = [];
	const context = {
		currentTime: 10,
		destination: {} as AudioDestinationNode,
		createBufferSource() {
			const source = fakeSource();
			sources.push(source);
			return source as unknown as AudioBufferSourceNode;
		},
		createGain() {
			const gain = fakeGain();
			gains.push(gain);
			return gain as unknown as GainNode;
		}
	};
	const scheduler = createBufferScheduler(context);
	return { context, sources, gains, scheduler };
}

describe('decoded buffer scheduler', () => {
	it('starts the next buffer exactly when the first ends by default', () => {
		const { context, sources, gains, scheduler } = setup();
		const firstBuffer = syntheticBuffer(3);
		const nextBuffer = syntheticBuffer(4);
		scheduler.play(firstBuffer, nextBuffer);

		const [first, next] = sources;
		const [[start]] = first.start.mock.calls;
		expect(start).toBeGreaterThan(context.currentTime);
		expect(next.start).toHaveBeenCalledExactlyOnceWith(start + firstBuffer.duration);
		expect(first.buffer).toBe(firstBuffer);
		expect(next.buffer).toBe(nextBuffer);
		for (const [index, gain] of gains.entries()) {
			expect(sources[index].connect).toHaveBeenCalledExactlyOnceWith(gain);
			expect(gain.connect).toHaveBeenCalledExactlyOnceWith(context.destination);
			expect(gain.gain.setValueAtTime).not.toHaveBeenCalled();
			expect(gain.gain.linearRampToValueAtTime).not.toHaveBeenCalled();
		}
	});

	it.each([
		{ first: 12, next: 14, request: 1, overlap: 1 },
		{ first: 12, next: 14, request: 60, overlap: 5 },
		{ first: 2, next: 12, request: 60, overlap: 1 },
		{ first: 12, next: 2, request: 60, overlap: 1 }
	])('uses complementary gain ramps for $overlap s with a $request s request ($first s into $next s)', ({
		first, next, request, overlap
	}) => {
		const { sources, gains, scheduler } = setup();
		scheduler.play(syntheticBuffer(first), syntheticBuffer(next), request);

		const [[start]] = sources[0].start.mock.calls;
		const end = start + first;
		const nextStart = end - overlap;
		expect(sources[1].start).toHaveBeenCalledExactlyOnceWith(nextStart);
		expect(gains[0].gain.setValueAtTime).toHaveBeenCalledExactlyOnceWith(1, nextStart);
		expect(gains[0].gain.linearRampToValueAtTime).toHaveBeenCalledExactlyOnceWith(0, end);
		expect(gains[1].gain.setValueAtTime).toHaveBeenCalledExactlyOnceWith(0, nextStart);
		expect(gains[1].gain.linearRampToValueAtTime).toHaveBeenCalledExactlyOnceWith(1, end);
	});

	it.each([-1, NaN, Infinity])('uses adjacent playback for an invalid fade duration of %s', (fade) => {
		const { sources, gains, scheduler } = setup();
		scheduler.play(syntheticBuffer(3), syntheticBuffer(4), fade);

		const [[start]] = sources[0].start.mock.calls;
		expect(sources[1].start).toHaveBeenCalledExactlyOnceWith(start + 3);
		expect(gains[0].gain.linearRampToValueAtTime).not.toHaveBeenCalled();
		expect(gains[1].gain.linearRampToValueAtTime).not.toHaveBeenCalled();
	});

	it('stops a playing buffer and a future start and disconnects both paths', () => {
		const { context, sources, gains, scheduler } = setup();
		scheduler.play(syntheticBuffer(4), syntheticBuffer(4), 1);
		context.currentTime = 11;
		expect(sources[0].start.mock.calls[0][0]).toBeLessThan(context.currentTime);
		expect(sources[1].start.mock.calls[0][0]).toBeGreaterThan(context.currentTime);
		scheduler.stop();
		scheduler.stop();

		for (const [index, source] of sources.entries()) {
			expect(source.stop).toHaveBeenCalledExactlyOnceWith();
			expect(source.onended).toBeNull();
			expect(source.disconnect).toHaveBeenCalledOnce();
			expect(gains[index].disconnect).toHaveBeenCalledOnce();
		}
	});

	it('cancels the old pair on replacement and isolates its late ended callbacks', () => {
		const { context, sources, gains, scheduler } = setup();
		scheduler.play(syntheticBuffer(4), syntheticBuffer(4));
		const oldCallbacks = sources.map((source) => source.onended);
		context.currentTime = 11;
		const replacement = syntheticBuffer(2);
		scheduler.play(replacement);
		for (const callback of oldCallbacks) callback?.();

		const [first, next, current] = sources;
		expect(first.stop).toHaveBeenCalledOnce();
		expect(next.stop).toHaveBeenCalledOnce();
		expect(current.buffer).toBe(replacement);
		expect(current.start.mock.calls[0][0]).toBeGreaterThan(context.currentTime);
		expect(current.disconnect).not.toHaveBeenCalled();
		expect(current.stop).not.toHaveBeenCalled();
		current.onended?.();
		scheduler.stop();
		expect(current.stop).not.toHaveBeenCalled();
		expect(current.disconnect).toHaveBeenCalledOnce();
		expect(gains[2].disconnect).toHaveBeenCalledOnce();
	});

	it('rolls back if the second source cannot start', () => {
		const { context, sources, gains, scheduler } = setup();
		const createSource = context.createBufferSource;
		vi.spyOn(context, 'createBufferSource').mockImplementation(() => {
			const source = createSource();
			if (sources.length === 2) {
				sources[1].start.mockImplementation(() => { throw new Error('start failed'); });
			}
			return source;
		});

		expect(() => scheduler.play(syntheticBuffer(1), syntheticBuffer(1))).toThrow('start failed');
		expect(sources[0].stop).toHaveBeenCalledOnce();
		expect(sources[1].stop).not.toHaveBeenCalled();
		for (const [index, source] of sources.entries()) {
			expect(source.disconnect).toHaveBeenCalledOnce();
			expect(gains[index].disconnect).toHaveBeenCalledOnce();
		}
	});
});
