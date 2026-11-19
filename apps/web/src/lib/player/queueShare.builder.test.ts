import { describe, expect, it, vi } from 'vitest';
import { createQueue, type Track } from './queue.js';
import { createQueueShare } from './queueShare.js';

const track = (id: string, title = id): Track => ({ id, title, artist: '', streamUrl: '/audio' });

function setup() {
	let queue = createQueue([track('a', 'First'), track('b')]);
	let position = 15.9;
	let clipboard: { writeText: (text: string) => Promise<void> } | undefined;
	let origin = () => 'https://example.test';
	const outcomes: { status: string; fallback: string | null; queue: typeof queue }[] = [];
	const share = createQueueShare({
		getQueue: () => queue,
		getPosition: () => position,
		getOrigin: () => origin(),
		getClipboard: () => clipboard,
		shortTitle: (title) => title,
		onOutcome: (status, fallback, forQueue) => outcomes.push({ status, fallback, queue: forQueue })
	});
	return {
		share, outcomes,
		setQueue: (next: typeof queue) => { queue = next; },
		setPosition: (next: number) => { position = next; },
		setClipboard: (next: typeof clipboard) => { clipboard = next; },
		setOrigin: (next: typeof origin) => { origin = next; },
		last: () => outcomes[outcomes.length - 1]
	};
}

function deferred() {
	let resolve!: () => void;
	let reject!: () => void;
	const promise = new Promise<void>((ok, fail) => { resolve = ok; reject = fail; });
	return { promise, resolve, reject };
}

describe('queue share lifecycle', () => {
	it('sets synchronous fallback and uses live position only for the current row', async () => {
		const s = setup();
		const pending = s.share.copy('a');
		expect(s.last().fallback).toBe('https://example.test/library?track=a&t=15');
		await pending;
		s.setPosition(99);
		await s.share.copy('b');
		expect(s.last().fallback).toBe('https://example.test/library?track=b&t=0');
		s.setPosition(Number.NaN);
		await s.share.copy('a');
		expect(s.last().fallback).toContain('t=0');
	});

	it('reports URL failure synchronously and binds clipboard receiver', async () => {
		const s = setup();
		s.setOrigin(() => { throw new Error('origin'); });
		const failed = s.share.copy('a');
		expect(s.last().status).toContain('First is no longer');
		await failed;
		s.setOrigin(() => 'https://example.test');
		const clipboard = { writeText: vi.fn(function (this: unknown) {
			expect(this).toBe(clipboard);
			return Promise.resolve();
		}) };
		s.setClipboard(clipboard);
		await s.share.copy('a');
		expect(s.last().status).toBe('Copied link for First.');
	});

	it('checks row existence after asynchronous success or failure, including duplicates', async () => {
		for (const reject of [false, true]) {
			const s = setup();
			const pending = deferred();
			s.setClipboard({ writeText: () => pending.promise });
			const copy = s.share.copy('a');
			s.setQueue(createQueue([track('b')]));
			if (reject) pending.reject(); else pending.resolve();
			await copy;
			expect(s.last().status).toContain('First is no longer');
			expect(s.last().fallback).toBeNull();
		}
		const s = setup();
		const pending = deferred();
		s.setClipboard({ writeText: () => pending.promise });
		const copy = s.share.copy('a');
		const next = createQueue([track('a', 'Duplicate')]);
		s.setQueue(next);
		pending.resolve();
		await copy;
		expect(s.last()).toEqual({ status: 'Copied link for First.', fallback: null, queue: next });
	});

	it('fences late completions on newer activation, reset and disposal', async () => {
		for (const invalidate of ['copy', 'reset', 'dispose'] as const) {
			const s = setup();
			const pending = deferred();
			s.setClipboard({ writeText: () => pending.promise });
			const copy = s.share.copy('a');
			if (invalidate === 'copy') { s.setClipboard(undefined); void s.share.copy('b'); }
			else if (invalidate === 'reset') s.share.reset();
			else s.share.dispose();
			const count = s.outcomes.length;
			pending.resolve();
			await copy;
			expect(s.outcomes).toHaveLength(count);
		}
	});

	it('handles a synchronous write failure without delaying feedback', async () => {
		const s = setup();
		s.setClipboard({ writeText: () => { throw new Error('denied'); } });
		const copy = s.share.copy('a');
		expect(s.last().fallback).toBe('https://example.test/library?track=a&t=15');
		await copy;
	});

	it('uses first matching title and the current duplicate position', async () => {
		const s = setup();
		const duplicates = createQueue([track('a', 'First'), track('a', 'Second')]);
		duplicates.currentIndex = 1;
		s.setQueue(duplicates);
		const clipboard = { writeText: vi.fn(() => Promise.resolve()) };
		s.setClipboard(clipboard);
		await s.share.copy('a');
		expect(clipboard.writeText).toHaveBeenCalledWith('https://example.test/library?track=a&t=15');
		expect(s.last().status).toBe('Copied link for First.');
	});

	it('reports a rejected write with a fallback for the latest queue', async () => {
		const s = setup();
		s.setClipboard({ writeText: () => Promise.reject(new Error('denied')) });
		const copy = s.share.copy('a');
		const next = createQueue([track('a')]);
		s.setQueue(next);
		await copy;
		expect(s.last()).toEqual({ status: '', fallback: 'https://example.test/library?track=a&t=15', queue: next });
	});
});
