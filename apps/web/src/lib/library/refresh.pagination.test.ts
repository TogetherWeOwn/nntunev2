import { describe, expect, it, vi } from 'vitest';
import { createLibraryRefresher, LIBRARY_BODY_MAX_BYTES } from './refresh.js';

function setup() {
	const invalidateLibraryCache = vi.fn();
	const refresher = createLibraryRefresher({
		sessionStorage: () => null,
		invalidateLibraryCache,
		timeoutMs: 5_000
	});
	return { refresher, invalidateLibraryCache };
}

function page(items: unknown[], next_cursor: string | null = null): Response {
	return Response.json({ items, next_cursor });
}

function pages(...responses: Response[]) {
	return vi.fn<typeof fetch>().mockImplementation(async () => {
		const response = responses.shift();
		if (!response) throw new Error('unexpected additional page');
		return response;
	});
}

describe('Library keyset continuation', () => {
	it('reaches more than 50 shared rows and a private row, preserving order', async () => {
		const { refresher } = setup();
		const shared = Array.from({ length: 55 }, (_, i) => ({ id: `shared-${i}` }));
		const fetcher = pages(
			page(shared.slice(0, 50), 'cursor_1'),
			page([...shared.slice(50), { id: 'private' }])
		);
		await refresher.refresh(fetcher);
		expect(fetcher.mock.calls.map(([url]) => url)).toEqual([
			'/api/library', '/api/library?cursor=cursor_1'
		]);
		expect(refresher.state.items.map((item) => item.id)).toEqual([
			...shared.map((item) => item.id), 'private'
		]);
		expect(refresher.state.loadError).toBeNull();
		const firstSignal = fetcher.mock.calls[0][1]?.signal;
		expect(firstSignal).toBeInstanceOf(AbortSignal);
		expect(fetcher.mock.calls[1][1]).toEqual({ credentials: 'include', signal: firstSignal });
	});

	it('does not publish a partial traversal after a failed continuation', async () => {
		const { refresher } = setup();
		await refresher.refresh(pages(page([{ id: 'previous' }])));
		await refresher.refresh(pages(
			page([{ id: 'partial' }], 'cursor_1'),
			new Response(null, { status: 503 })
		));
		expect(refresher.state.items.map((item) => item.id)).toEqual(['previous']);
		expect(refresher.state.staleAt).not.toBeNull();
	});

	it('clears prior and partial items on an unauthenticated continuation', async () => {
		const { refresher, invalidateLibraryCache } = setup();
		await refresher.refresh(pages(page([{ id: 'previous' }])));
		await refresher.refresh(pages(
			page([{ id: 'partial' }], 'cursor_1'),
			new Response(null, { status: 401 })
		));
		expect(refresher.state.items).toEqual([]);
		expect(refresher.state.requiresSignIn).toBe(true);
		expect(invalidateLibraryCache).toHaveBeenCalledOnce();
	});

	it('refuses cyclic cursors without committing partial items', async () => {
		const { refresher } = setup();
		const fetcher = pages(
			page([{ id: 'one' }], 'cursor_1'),
			page([{ id: 'two' }], 'cursor_1')
		);
		await refresher.refresh(fetcher);
		expect(fetcher).toHaveBeenCalledTimes(2);
		expect(refresher.state.items).toEqual([]);
		expect(refresher.state.loadError).not.toBeNull();
	});

	it('applies the retained-byte cap across the entire traversal, not per page', async () => {
		const { refresher } = setup();
		const title = 'x'.repeat(Math.floor(LIBRARY_BODY_MAX_BYTES * 0.6));
		await refresher.refresh(pages(
			page([{ id: 'one', title }], 'cursor_1'),
			page([{ id: 'two', title }])
		));
		expect(refresher.state.items).toEqual([]);
		expect(refresher.state.loadError).not.toBeNull();
	});

	it('auth transitions fence late continuation responses', async () => {
		const { refresher } = setup();
		let resolve!: (response: Response) => void;
		let started!: () => void;
		const continuationStarted = new Promise<void>((done) => { started = done; });
		const fetcher = vi.fn<typeof fetch>()
			.mockResolvedValueOnce(page([{ id: 'one' }], 'cursor_1'))
			.mockImplementationOnce(() => {
				started();
				return new Promise<Response>((done) => { resolve = done; });
			});
		const loading = refresher.refresh(fetcher);
		await continuationStarted;
		refresher.handleLocalAuthTransition();
		resolve(page([{ id: 'late' }]));
		await loading;
		expect(refresher.state.items).toEqual([]);
		expect(refresher.state.requiresSignIn).toBe(true);
	});
});
