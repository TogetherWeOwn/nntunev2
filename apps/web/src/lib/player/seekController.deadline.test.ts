import { describe, expect, it, vi } from 'vitest';

import { SeekController, type SeekFetch } from './seekController.js';

/**
 * Deadline acceptance for Range seeks (`seekController.deadline`).
 *
 * Today's `SeekController.seek` aborts only when a newer seek supersedes it.
 * If headers or the body never complete, the seek stays pending forever.
 * The constructor-injected deadline under test closes that hang: a seek that
 * outlives its deadline must reject with a `DOMException` named
 * `TimeoutError`, abort its request signal, and release its unread body.
 *
 * RED notes (no timer implemented yet):
 * - The deadline is injected as a third constructor argument
 *   `{ deadlineMs }`. `SeekController` takes only `(url, fetchFn)` today, so
 *   the suite reaches the future constructor through a cast: typecheck stays
 *   green while the extra argument is ignored at runtime and every hang case
 *   fails at the assertion level.
 * - Every hang case races the seek against a wall-clock guard
 *   (`withGuard`). When today's code never times out, the guard expires and
 *   the test fails with `test guard expired ...` instead of hanging the
 *   vitest process. After the timer lands, the seek must reject with
 *   `TimeoutError` long before the guard fires.
 * - Nothing here inspects source text or edits existing seek suites.
 */

type DeadlineOptions = { deadlineMs: number };
type SeekControllerWithDeadline = new (
	url: string,
	fetchFn?: SeekFetch,
	options?: DeadlineOptions
) => SeekController;

function withDeadline(url: string, fetchFn: SeekFetch, deadlineMs: number): SeekController {
	return new (SeekController as unknown as SeekControllerWithDeadline)(url, fetchFn, {
		deadlineMs
	});
}

/** Race a seek against a wall-clock guard so a missing deadline fails instead of hanging. */
function withGuard<T>(promise: Promise<T>, ms: number, label: string): Promise<T> {
	let timer: ReturnType<typeof setTimeout> | undefined;
	const guard = new Promise<never>((_resolve, reject) => {
		timer = setTimeout(
			() =>
				reject(
					new Error(
						`test guard expired: ${label} — seek never settled (no deadline in today's code)`
					)
				),
			ms
		);
	});
	return Promise.race([promise, guard]).finally(() => {
		clearTimeout(timer);
	});
}

function rangeResponse(header: string): Response {
	const match = /^bytes=(\d+)-(\d*)$/.exec(header);
	if (!match) throw new Error(`unexpected range ${header}`);
	const start = Number(match[1]);
	const total = 262_144;
	const end = match[2] === '' ? total - 1 : Number(match[2]);
	return new Response(new Uint8Array(end - start + 1), {
		status: 206,
		headers: { 'content-range': `bytes ${start}-${end}/${total}` }
	});
}

/** A fetch whose headers never arrive; it settles only when its signal aborts. */
function headerHangFetch(signals: AbortSignal[]): SeekFetch {
	return (_url, { signal }) => {
		signals.push(signal);
		return new Promise<Response>((_resolve, reject) => {
			signal.addEventListener('abort', () => {
				reject(new DOMException('The operation was aborted.', 'AbortError'));
			});
		});
	};
}

const URL = 'https://audio.example/track';
const SHORT_DEADLINE_MS = 50;
const GUARD_MS = 1_000;

describe('SeekController deadline', () => {
	it('completes a fast seek before the deadline (control)', async () => {
		const controller = withDeadline(
			URL,
			async (_url, { headers }) => rangeResponse(headers.range),
			1_000
		);

		const result = await withGuard(controller.seek(0, 99), GUARD_MS, 'fast control');

		expect(result.status).toBe(206);
		expect(result.bytes).toHaveLength(100);
		expect(controller.completed).toBe(1);
		expect(controller.pending).toBe(false);
	});

	it('rejects a never-settling fetch with TimeoutError, aborts the signal, and clears pending', async () => {
		const signals: AbortSignal[] = [];
		const controller = withDeadline(URL, headerHangFetch(signals), SHORT_DEADLINE_MS);

		const pending = controller.seek(0, 99);
		void pending.catch(() => {});
		let error: unknown;
		try {
			await withGuard(pending, GUARD_MS, 'header hang');
		} catch (caught) {
			error = caught;
		} finally {
			controller.abort();
		}

		// Distinguishable: TimeoutError, not supersede-AbortError nor an HTTP error.
		expect(error).toBeInstanceOf(DOMException);
		expect((error as DOMException).name).toBe('TimeoutError');
		expect((error as DOMException).name).not.toBe('AbortError');
		expect((error as Error).message).not.toMatch(/HTTP/);
		expect((error as Error).message).not.toMatch(/superseded/);
		expect(signals).toHaveLength(1);
		expect(signals[0].aborted).toBe(true);
		expect(controller.pending).toBe(false);
		expect(controller.completed).toBe(0);
	});

	it('releases the unread body when the body never completes', async () => {
		let cancels = 0;
		const stream = new ReadableStream<Uint8Array>({
			pull() {
				// Never enqueue and never close: `read()` pends until the deadline fires.
			},
			cancel() {
				cancels += 1;
				return undefined;
			}
		});
		const signals: AbortSignal[] = [];
		const fetchFn: SeekFetch = async (_url, { signal }) => {
			signals.push(signal);
			return new Response(stream, {
				status: 206,
				headers: { 'content-range': 'bytes 0-99/1000' }
			});
		};
		const controller = withDeadline(URL, fetchFn, SHORT_DEADLINE_MS);

		const pending = controller.seek(0, 99);
		void pending.catch(() => {});
		let error: unknown;
		try {
			await withGuard(pending, GUARD_MS, 'body hang');
		} catch (caught) {
			error = caught;
		} finally {
			controller.abort();
		}

		expect(error).toBeInstanceOf(DOMException);
		expect((error as DOMException).name).toBe('TimeoutError');
		expect((error as Error).message).not.toMatch(/HTTP/);
		expect(signals).toHaveLength(1);
		expect(signals[0].aborted).toBe(true);
		// The unread body must be released, not left streaming into nothing.
		await vi.waitFor(() => expect(cancels).toBeGreaterThanOrEqual(1), {
			timeout: GUARD_MS,
			interval: 10
		});
		expect(stream.locked).toBe(false);
		expect(controller.pending).toBe(false);
		expect(controller.completed).toBe(0);
	});

	it('a newer seek still supersedes and rejects the older one with AbortError', async () => {
		const signals: AbortSignal[] = [];
		let calls = 0;
		const fetchFn: SeekFetch = (_url, init) => {
			calls += 1;
			if (calls === 1) return headerHangFetch(signals)(_url, init);
			signals.push(init.signal);
			return Promise.resolve(rangeResponse(init.headers.range));
		};
		// Generous deadline: supersede must win on its own, not via the timer.
		const controller = withDeadline(URL, fetchFn, 10_000);

		const first = controller.seek(0, 99);
		void first.catch(() => {});
		const second = await withGuard(controller.seek(200, 299), GUARD_MS, 'superseding seek');

		await expect(first).rejects.toMatchObject({ name: 'AbortError' });
		expect(second.status).toBe(206);
		expect(second.bytes).toHaveLength(100);
		expect(controller.aborted).toBe(1);
		expect(controller.generation).toBe(2);
		expect(controller.pending).toBe(false);
		controller.abort();
	});
});
