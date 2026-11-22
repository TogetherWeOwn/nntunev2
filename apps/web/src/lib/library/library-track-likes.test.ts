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

function listing(sessionBinding = A, signal = new AbortController().signal): LibraryRefreshUpdate {
	const isCurrent = () => true, runIfCurrent = async <T,>(work: () => T) => work();
	const accountSession = { generation: sessionBinding, signal, isCurrent, run: runIfCurrent };
	return { sessionBinding, accountSession, authoritative: true, authBoundary: false, isCurrent, runIfCurrent };
}

function setup() {
	const fetcher = vi.fn<typeof fetch>();
	const tasteRead = vi.fn<typeof fetch>(async () => list(A));
	vi.stubGlobal('fetch', (input: string, init: RequestInit) => (
		input.endsWith('/v1/me/taste/session') && init.method === 'GET' ? tasteRead : fetcher
	)(input, init));
	return { likes: createLibraryTrackLikes(), fetcher, tasteRead };
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

it.each([A, B])('fences an earlier write when refreshing with binding %s', async (binding) => {
	const { likes, fetcher } = setup();
	fetcher.mockResolvedValueOnce(list(A)).mockResolvedValueOnce(session(A));
	const account = new AbortController();
	await likes.publish(listing(A, account.signal), tracks);
	const result = Promise.withResolvers<Response>(), dispatched = Promise.withResolvers<void>();
	fetcher.mockImplementationOnce(() => { dispatched.resolve(); return result.promise; });
	const old = likes.controls(track)!;
	const toggle = old.toggle();
	await dispatched.promise;
	account.abort();
	likes.invalidate();
	expect(likes.controls(track)).toBeUndefined();
	likes.retire();
	const sameAccount = binding === A;
	fetcher.mockResolvedValueOnce(list(binding, sameAccount ? [liked] : [])).mockResolvedValueOnce(session(binding));
	const refresh = likes.publish(listing(binding), tracks);
	if (sameAccount) {
		await old.toggle();
		expect(fetcher).toHaveBeenCalledTimes(3);
		expect(fetcher.mock.calls[2][1]?.signal?.aborted).toBe(false);
		expect(likes.controls(track)).toBeUndefined();
		expect(likes.feedback).toContain('Refresh the library');
	} else {
		await refresh; // The new account must not wait for the previous one.
	}
	result.resolve(Response.json({ target_id: track, liked_at: 1 }));
	await Promise.all([toggle, refresh]);
	expect(likes.controls(track)?.liked).toBe(sameAccount);
	expect(likes.feedback).toBe('');
	expect(fetcher).toHaveBeenCalledTimes(5);
});

const target = { kind: 'track', target_id: track };
const hide = { ...target, signal: 'hide', until: null };
const tasteReply = (row = hide, binding = A) => Response.json({ ...row, session_binding: binding });

it('uses explicit Hide over conflicting Likes and explicit Love outside a full Likes list', async () => {
	const { likes, fetcher, tasteRead } = setup();
	const loved = tracks[1].id, omitted = 'Job_C';
	const others = Array.from({ length: MAX_LIKES - 2 }, (_, i) => ({ ...liked, target_id: `Other_${i}` }));
	fetcher.mockResolvedValueOnce(list(A, [liked, { ...liked, target_id: omitted }, ...others])).mockResolvedValueOnce(session(A));
	tasteRead.mockResolvedValueOnce(list(A, [hide, ...[...others, { ...target, target_id: loved }]
		.map((item) => ({ ...item, signal: 'love', until: null }))]));
	await likes.publish(listing(), [...tracks, { id: omitted }]);
	expect(likes.controls(track)).toMatchObject({ liked: false, taste: { saved: 'hide' } });
	expect(likes.controls(loved)).toMatchObject({ liked: true, taste: { saved: undefined } });
	expect(likes.controls(omitted)).toMatchObject({ liked: true, taste: undefined });

	fetcher.mockResolvedValueOnce(Response.json({ ...target, cleared: true, session_binding: A })).mockResolvedValueOnce(session(A));
	await likes.controls(track)!.taste!.set(null);
	expect(likes.controls(track)).toMatchObject({ liked: false, taste: { saved: undefined } });
	fetcher.mockResolvedValueOnce(Response.json({ target_id: omitted, liked: false }));
	await likes.controls(omitted)!.toggle();
	expect(likes.controls(omitted)).toMatchObject({ liked: false, taste: { saved: undefined } });
});

it('withholds taste data from a mismatched read without changing verified Likes', async () => {
	const { likes, fetcher, tasteRead } = setup();
	tasteRead.mockResolvedValueOnce(list(B, [hide]));
	fetcher.mockResolvedValueOnce(list(A, [liked])).mockResolvedValueOnce(session(A));
	await likes.publish(listing(), tracks);
	expect(tasteRead).toHaveBeenCalledWith(expect.stringMatching(/\/v1\/me\/taste\/session$/), expect.objectContaining({
		credentials: 'include', headers: expect.objectContaining({ 'x-nntune-session-binding': A })
	}));
	expect(likes.controls(track)).toMatchObject({ liked: true, taste: undefined });
});

it('serializes preference writes with Like across aliases, validates the receipt and retries only on intent', async () => {
	const { likes, fetcher } = setup();
	const mbid = 'abcd0000-0000-4000-8000-000000000001', alias = mbid.toUpperCase();
	const signal = { ...hide, target_id: mbid };
	fetcher.mockResolvedValueOnce(list(A, [{ ...liked, target_id: mbid }])).mockResolvedValueOnce(session(A));
	await likes.publish(listing(), [{ id: mbid }, { id: alias, like_target_id: mbid }]);
	const reply = Promise.withResolvers<Response>();
	fetcher.mockReturnValueOnce(reply.promise);
	const save = likes.controls(alias)!.taste!.set('hide');
	expect(likes.controls(mbid)).toMatchObject({ liked: true, pending: true });
	await likes.controls(mbid)!.toggle();
	reply.resolve(tasteReply({ ...signal, target_id: alias }));
	await save;
	expect(fetcher).toHaveBeenCalledTimes(3);
	expect(likes.feedback).toContain('Could not confirm');
	expect(likes.controls(mbid)).toMatchObject({ liked: true, pending: false });
	fetcher.mockResolvedValueOnce(tasteReply(signal)).mockResolvedValueOnce(session(A));
	await likes.controls(alias)!.taste!.set('hide');
	expect(fetcher).toHaveBeenNthCalledWith(4, expect.stringMatching(/\/v1\/me\/taste\/session$/), expect.objectContaining({
		method: 'PUT', body: JSON.stringify({ ...target, target_id: mbid, signal: 'hide' }),
		headers: expect.objectContaining({ 'x-nntune-session-binding': A })
	}));
	expect(likes.controls(mbid)).toMatchObject({ liked: false, taste: { saved: 'hide' } });
});

it.each(['receipt', 'verification', 'new owner', 'server refusal'] as const)('never publishes unproven taste success (%s)', async (boundary) => {
	const { likes, fetcher, tasteRead } = setup();
	fetcher.mockResolvedValueOnce(list(A, [liked])).mockResolvedValueOnce(session(A));
	await likes.publish(listing(), tracks);
	const reply = Promise.withResolvers<Response>(), dispatched = Promise.withResolvers<void>();
	fetcher.mockImplementationOnce(() => { dispatched.resolve(); return reply.promise; });
	const save = likes.controls(track)!.taste!.set('hide');
	await dispatched.promise;
	if (boundary === 'new owner') {
		tasteRead.mockResolvedValueOnce(list(B));
		fetcher.mockResolvedValueOnce(list(B, [liked])).mockResolvedValueOnce(session(B));
		await likes.publish(listing(B), tracks);
	} else if (boundary === 'verification') fetcher.mockResolvedValueOnce(session(B));
	reply.resolve(boundary === 'server refusal' ? Response.json({}, { status: 412 }) : tasteReply(hide, boundary === 'receipt' ? B : A));
	await save;
	expect(fetcher).toHaveBeenCalledTimes(boundary === 'new owner' ? 5 : boundary === 'verification' ? 4 : 3);
	expect(likes.controls(track)?.taste?.saved).toBeUndefined();
	expect(likes.controls(track)?.liked).toBe(boundary === 'verification' || boundary === 'server refusal' ? undefined : true);
	expect(likes.feedback).not.toContain('saved');
});

it('honors snooze expiry and accepts a proven idempotent clear without a Like', async () => {
	const { likes, fetcher, tasteRead } = setup();
	const now = Math.floor(Date.now() / 1000);
	tasteRead.mockResolvedValueOnce(list(A, [{ ...target, signal: 'snooze', until: now - 1 }]));
	fetcher.mockResolvedValueOnce(list(A)).mockResolvedValueOnce(session(A));
	await likes.publish(listing(), tracks);
	expect(likes.controls(track)!.taste!.saved).toBeUndefined();
	await likes.controls(track)!.taste!.set(null);
	expect(fetcher).toHaveBeenCalledTimes(2);
	fetcher.mockResolvedValueOnce(Response.json({ ...target, signal: 'snooze', until: now + 30 * 86400, session_binding: A }))
		.mockResolvedValueOnce(session(A));
	await likes.controls(track)!.taste!.set('snooze');
	expect(fetcher.mock.calls[2][1]?.body).toBe(JSON.stringify({ ...target, signal: 'snooze', snooze_seconds: 2592000 }));
	expect(likes.controls(track)!.taste!.saved).toBe('snooze');
	fetcher.mockResolvedValueOnce(Response.json({ ...target, cleared: false, session_binding: A })).mockResolvedValueOnce(session(A));
	await likes.controls(track)!.taste!.set(null);
	expect(fetcher.mock.calls[4][1]).toMatchObject({ method: 'DELETE', body: JSON.stringify(target),
		headers: { 'content-type': 'application/json', 'x-nntune-session-binding': A } });
	expect(likes.controls(track)).toMatchObject({ liked: false, taste: { saved: undefined } });
});
