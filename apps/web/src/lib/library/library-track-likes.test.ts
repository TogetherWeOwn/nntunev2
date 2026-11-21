import { afterEach, expect, it, vi } from 'vitest';
import { createLibraryTrackLikes } from './library-track-likes.svelte.js';
import { MAX_LIKES } from './likes.js';
import type { LibraryRefreshUpdate } from './refresh.js';

const A = 'A'.repeat(43), B = 'E'.repeat(43);
const tracks = [{ id: 'Job_A' }, { id: 'Job_B' }];
const track = tracks[0].id;
const liked = { kind: 'track', target_id: track, created_at: 1 };
const session = (binding: string) => Response.json({ session_binding: binding });
const list = (binding: string, items: unknown[] = []) => Response.json({ session_binding: binding, items });

function listing(sessionBinding = A): LibraryRefreshUpdate {
	return { sessionBinding, authoritative: true, authBoundary: false, isCurrent: () => true, runIfCurrent: async (work) => work() };
}

function setup() {
	const fetcher = vi.fn<typeof fetch>();
	vi.stubGlobal('fetch', fetcher);
	return { likes: createLibraryTrackLikes(), fetcher };
}

afterEach(() => vi.unstubAllGlobals());

it('disables unknown and nonmember rows on a full response', async () => {
	const { likes, fetcher } = setup();
	fetcher.mockResolvedValueOnce(list(A, [liked, ...Array.from({ length: MAX_LIKES - 1 }, (_, index) => (
		{ ...liked, target_id: `Other_${index}` }
	))])).mockResolvedValueOnce(session(A));
	await likes.publish(listing(), tracks);
	expect(likes.controls(track)?.liked).toBe(true);
	expect(likes.controls(tracks[1].id)).toBeUndefined();
	expect(likes.controls('Other_0')).toBeUndefined();
});

it.each(['mismatched read binding', 'mismatched verifier', 'retired publication'] as const)(
	'withholds controls after %s', async (scenario) => {
		const { likes, fetcher } = setup();
		const verifier = Promise.withResolvers<Response>(), dispatched = Promise.withResolvers<void>();
		fetcher.mockResolvedValueOnce(list(scenario === 'mismatched read binding' ? B : A, [liked]));
		fetcher.mockImplementationOnce(() => { dispatched.resolve(); return verifier.promise; });
		const load = likes.publish(listing(), tracks);
		if (scenario !== 'mismatched read binding') {
			await dispatched.promise;
			if (scenario === 'retired publication') likes.clear();
			verifier.resolve(session(scenario === 'mismatched verifier' ? B : A));
		}
		await load;
		expect(likes.controls(track)).toBeUndefined();
		expect(!!likes.feedback).toBe(scenario !== 'retired publication');
	}
);

it('rereads the same account after its earlier write settles, without accepting an old control', async () => {
	const { likes, fetcher } = setup();
	fetcher.mockResolvedValueOnce(list(A)).mockResolvedValueOnce(session(A));
	await likes.publish(listing(), tracks);
	const result = Promise.withResolvers<Response>(), dispatched = Promise.withResolvers<void>();
	fetcher.mockImplementationOnce(() => { dispatched.resolve(); return result.promise; });
	const old = likes.controls(track)!;
	const toggle = old.toggle();
	await dispatched.promise;
	likes.invalidate();
	expect(likes.controls(track)).toBeUndefined();
	likes.retire(); // A new Library refresh has started, but is not yet verified.
	fetcher.mockResolvedValueOnce(list(A, [liked])).mockResolvedValueOnce(session(A));
	const refresh = likes.publish(listing(), tracks);
	await old.toggle();
	expect(fetcher).toHaveBeenCalledTimes(3);
	expect(likes.controls(track)).toBeUndefined();
	expect(likes.feedback).toContain('Refresh the library');
	result.resolve(Response.json({ target_id: track, liked_at: 1 }));
	await Promise.all([toggle, refresh]);
	expect(likes.controls(track)?.liked).toBe(true);
	expect(likes.feedback).toBe('');
	expect(fetcher).toHaveBeenCalledTimes(5);
});

it('does not wait for or publish a previous account write in the new account', async () => {
	const { likes, fetcher } = setup();
	fetcher.mockResolvedValueOnce(list(A)).mockResolvedValueOnce(session(A));
	await likes.publish(listing(), tracks);
	const result = Promise.withResolvers<Response>(), dispatched = Promise.withResolvers<void>();
	fetcher.mockImplementationOnce(() => { dispatched.resolve(); return result.promise; });
	const toggle = likes.controls(track)!.toggle();
	await dispatched.promise;
	fetcher.mockResolvedValueOnce(list(B)).mockResolvedValueOnce(session(B));
	await likes.publish(listing(B), tracks);
	result.resolve(Response.json({ target_id: track, liked_at: 1 }));
	await toggle;
	expect(likes.controls(track)?.liked).toBe(false);
	expect(likes.feedback).toBe('');
});

it('retires live controls on a rejected session-bound write', async () => {
	const { likes, fetcher } = setup();
	fetcher.mockResolvedValueOnce(list(A)).mockResolvedValueOnce(session(A));
	fetcher.mockResolvedValueOnce(Response.json({}, { status: 412 }));
	await likes.publish(listing(), tracks);
	const old = likes.controls(track)!;
	await old.toggle();
	await old.toggle();
	expect(likes.controls(track)).toBeUndefined();
	expect(likes.feedback).toContain('Refresh the library');
	expect(fetcher).toHaveBeenCalledTimes(3);
});
