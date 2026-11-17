import { expect, test } from 'vitest';
import { createPendingShareSeek } from './pendingShareSeek.js';

const target = { trackId: 'a', streamUrl: '/a', seconds: 90 };

function fixture() {
	let duration = NaN, position = 0, refuse = false, live = true, incarnation = 0;
	let onWrite = () => {}, onCancel = () => {};
	const element = {
		get duration() { return duration; },
		get currentTime() { return position; },
		set currentTime(value: number) {
			if (refuse) throw new Error('refused');
			position = value;
			onWrite();
		}
	} as HTMLAudioElement;
	const seek = createPendingShareSeek(
		(candidate) => {
			const captured = incarnation;
			return { isCurrent: () => live && candidate === element && captured === incarnation, cancelResume: () => onCancel() };
		},
		() => false
	);
	return {
		seek, element,
		duration: (value: number) => { duration = value; },
		refuse: (value: boolean) => { refuse = value; },
		live: (value: boolean) => { live = value; },
		reattach: () => { incarnation += 1; },
		onWrite: (value: () => void) => { onWrite = value; },
		onCancel: (value: () => void) => { onCancel = value; },
		position: () => position
	};
}

test('refused write retries and unknown duration clamps when metadata arrives', () => {
	const f = fixture();
	f.seek.set(target);
	f.refuse(true); f.seek.apply(f.element);
	expect(f.seek.current).not.toBeNull();
	f.refuse(false); f.seek.apply(f.element);
	expect(f.position()).toBe(90);
	expect(f.seek.current).not.toBeNull();
	f.duration(60); f.seek.apply(f.element);
	expect(f.position()).toBe(60);
	expect(f.seek.current).toBeNull();
});

test('replacement inside a setter survives the old application', () => {
	const f = fixture(); f.duration(100); f.seek.set(target);
	f.onWrite(() => f.seek.set({ ...target, seconds: 12 }));
	f.seek.apply(f.element);
	expect(f.seek.current?.seconds).toBe(12);
});

test('clear invalidates scheduled revisions and a newer share is not applied by an old tick', () => {
	const f = fixture(); const old = f.seek.set(target);
	f.seek.clear(); f.seek.set({ ...target, seconds: 12 });
	f.seek.apply(f.element, old);
	expect(f.position()).toBe(0);
	expect(f.seek.current?.seconds).toBe(12);
});

test('stale element and invalidated owner cannot write', () => {
	const f = fixture(); f.seek.set(target);
	f.seek.apply({} as HTMLAudioElement);
	f.live(false); f.seek.apply(f.element);
	expect(f.position()).toBe(0);
	expect(f.seek.current).not.toBeNull();
});

test('own synchronous seeking is ignored, a different native seek retires the adjustment', () => {
	const f = fixture(); f.seek.set(target);
	f.onWrite(() => f.seek.observeSeeking(f.element));
	f.seek.apply(f.element);
	expect(f.seek.current).not.toBeNull();
	f.onWrite(() => {}); f.element.currentTime = 4;
	f.seek.observeSeeking(f.element);
	expect(f.seek.current).toBeNull();
});

test('replacement during the seeking observer getter survives retirement', () => {
	const f = fixture(); f.seek.set(target); f.seek.apply(f.element);
	Object.defineProperty(f.element, 'currentTime', {
		get: () => { f.seek.set({ ...target, seconds: 12 }); return 4; }
	});
	f.seek.observeSeeking(f.element);
	expect(f.seek.current?.seconds).toBe(12);
	expect(f.seek.current?.appliedSeconds).toBeNull();
});

test('owner invalidation during the seeking observer getter preserves the adjustment', () => {
	const f = fixture(); f.seek.set(target); f.seek.apply(f.element);
	Object.defineProperty(f.element, 'currentTime', {
		get: () => { f.reattach(); return 4; }
	});
	f.seek.observeSeeking(f.element);
	expect(f.seek.current?.seconds).toBe(90);
	expect(f.seek.current?.appliedSeconds).toBe(90);
});

test('resume cancellation may invalidate the receipt before the write', () => {
	const f = fixture(); f.seek.set(target);
	f.onCancel(() => f.reattach());
	f.seek.apply(f.element);
	expect(f.position()).toBe(0);
	expect(f.seek.current).not.toBeNull();
});

test('throwing and mutating duration getters leave the newer transaction intact', () => {
	const f = fixture(); f.seek.set(target);
	Object.defineProperty(f.element, 'duration', { configurable: true, get: () => { throw Error('not ready'); } });
	f.seek.apply(f.element);
	expect(f.seek.current?.seconds).toBe(90);
	Object.defineProperty(f.element, 'duration', { get: () => { f.seek.set({ ...target, seconds: 3 }); return 100; } });
	f.seek.apply(f.element);
	expect(f.position()).toBe(0);
	expect(f.seek.current?.seconds).toBe(3);
});
