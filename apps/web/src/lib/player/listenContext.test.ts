import { describe, expect, it, vi } from 'vitest';
import { openListenContext } from './listenContext.js';

const first = 'a'.repeat(43);
const next = 'b'.repeat(43);

function response(context: string): Response {
	return new Response(JSON.stringify({ context }));
}

describe('listen context', () => {
	it('does not reacquire a context when the cookie changes without a transition signal', async () => {
		let issued = first;
		const fetchFn = vi.fn<typeof fetch>(async () => response(issued));
		const scope = openListenContext('https://api.test/', fetchFn);
		expect(scope.current()).toBeUndefined();
		await vi.waitFor(() => expect(scope.current()).toBe(first));

		issued = next;
		expect(scope.current()).toBe(first);
		expect(fetchFn).toHaveBeenCalledExactlyOnceWith(
			'https://api.test/v1/plays/events/context',
			expect.objectContaining({ method: 'POST', credentials: 'include', cache: 'no-store' })
		);
		scope.drop();
		expect(scope.current()).toBeUndefined();
	});

	it('drops a response whose request outlives the scope', async () => {
		let finish!: (value: Response) => void;
		const fetchFn = vi.fn<typeof fetch>(() => new Promise((resolve) => (finish = resolve)));
		const scope = openListenContext('', fetchFn);
		scope.drop();
		expect(fetchFn.mock.calls[0][1]?.signal?.aborted).toBe(true);

		const late = response(first);
		const read = vi.spyOn(late, 'json');
		finish(late);
		await vi.waitFor(() => expect(read).toHaveBeenCalledTimes(1));
		await read.mock.results[0].value;
		expect(scope.current()).toBeUndefined();
	});
});
