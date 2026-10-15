import { describe, expect, it } from 'vitest';

// Failing-first acceptance tests for TOG-25177 (built on TOG-25185).
// The Builder implements `./seek.js`; this file must NOT be edited on the
// build card — only the implementation file may change.
//
// Contract pinned here: rapid sequential seeks abort every superseded load
// so the player can never wedge. Firing N seeks without awaiting must settle
// the first N-1 as aborts and resolve only the last; the controller returns
// to idle at the final position.
import { createSeekController } from './seek.js';

function abortableGate() {
	let release!: () => void;
	const entered: AbortSignal[] = [];
	const gate = new Promise<void>((resolve) => {
		release = resolve;
	});
	const load = (signal: AbortSignal): Promise<void> => {
		entered.push(signal);
		if (signal.aborted) return Promise.reject(new DOMException('Aborted', 'AbortError'));
		return gate.then(() => {
			if (signal.aborted) throw new DOMException('Aborted', 'AbortError');
		});
	};
	return { entered, load, release };
}

describe('createSeekController', () => {
	it('aborts superseded seeks so only the last of a 10x storm resolves', async () => {
		const { entered, load, release } = abortableGate();
		const controller = createSeekController(load);

		const pending = Array.from({ length: 10 }, (_, index) => controller.seek(index * 5));
		release();
		const outcomes = await Promise.allSettled(pending);

		// First nine are superseded and must abort; the last one wins.
		expect(entered).toHaveLength(10);
		for (const outcome of outcomes.slice(0, 9)) {
			expect(outcome.status).toBe('rejected');
			if (outcome.status === 'rejected') {
				expect((outcome.reason as DOMException)?.name).toBe('AbortError');
			}
		}
		expect(outcomes[9]).toMatchObject({ status: 'fulfilled' });
	});

	it('returns to idle at the final position after a seek storm', async () => {
		const seen: number[] = [];
		const controller = createSeekController(async (_signal, positionSeconds) => {
			seen.push(positionSeconds);
		});

		await Promise.allSettled(Array.from({ length: 10 }, (_, index) => controller.seek(index * 5)));

		expect(controller.state).toBe('idle');
		expect(controller.positionSeconds).toBe(45);
		expect(seen.at(-1)).toBe(45);
	});

	it('reports seeking while a load is in flight', async () => {
		const { load, release } = abortableGate();
		const controller = createSeekController(load);

		const pending = controller.seek(30);
		expect(controller.state).toBe('seeking');
		release();
		await pending;

		expect(controller.state).toBe('idle');
		expect(controller.positionSeconds).toBe(30);
	});
});
