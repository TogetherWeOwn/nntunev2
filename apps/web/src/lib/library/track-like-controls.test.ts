import { render } from 'svelte/server';
import { describe, expect, it, vi } from 'vitest';
import TrackRows from '../../routes/library/TrackRows.svelte';
import { ApiClient } from '../api/client.js';
import PlayerQueue from '../player/PlayerQueue.svelte';
import { createQueue } from '../player/queue.js';
import type { LibraryRefreshUpdate } from './refresh.js';
import { createTrackLikeControls, type TrackLikeControls } from './track-like-controls.js';

const track = { id: 'Job_42', title: 'Night Drive', artist: '', streamUrl: '' };
const baseUrl = 'https://api.example.test';
const path = `${baseUrl}/v1/me/likes/track/${track.id}`;
const response = () => Response.json({ target_id: track.id, liked_at: 1 });

function readiness(admit: () => boolean | Promise<boolean> = () => true): LibraryRefreshUpdate {
	return {
		authoritative: true,
		authBoundary: false,
		isCurrent: () => true,
		runIfCurrent: async (work) => {
			if (!await admit()) return;
			const result = work();
			expect(result).not.toBeInstanceOf(Promise);
			return result;
		}
	};
}

function ownership() {
	let liked = false;
	let publication: LibraryRefreshUpdate | undefined = readiness();
	const fetchFn = vi.fn<typeof fetch>();
	return {
		fetchFn,
		publish: (next?: LibraryRefreshUpdate) => { publication = next; },
		owner: {
			client: new ApiClient({ baseUrl, fetchFn }),
			binding: 'A'.repeat(43),
			readiness: () => publication,
			liked: (id: string) => id === track.id ? liked : undefined,
			confirm: vi.fn((_id: string, _confirmedId: string, value: boolean) => { liked = value; }),
			failed: vi.fn()
		}
	};
}

function surfaces(likeControls?: TrackLikeControls, interactionsUnavailable = false) {
	const noop = () => {};
	return [
		render(TrackRows, { props: {
			tracks: [track], favoriteIds: new Set<string>(), queuedIds: new Set<string>(), selectedIds: [],
			likeControls, ontogglefavorite: noop, onrowclick: noop, onselectionclick: noop,
			onselectionchange: noop, ontrackdragstart: noop, ontrackdragend: noop,
			ontrackplay: noop, ontrackmenu: noop, oncopylink: noop
		} }).body,
		render(PlayerQueue, { props: {
			state: createQueue([track]), likeControls, ontogglefavorite: noop, interactionsUnavailable
		} }).body
	];
}

function expectLikeButtons(controls: TrackLikeControls, pressed: boolean, disabled: boolean) {
	for (const body of surfaces(controls)) {
		expect(body).toContain(`aria-label="Like Night Drive" aria-pressed="${pressed}" aria-disabled="${disabled}"`);
		expect(body).toContain('aria-label="Star Night Drive"');
	}
}

describe('injected track and queue Like controls', () => {
	it('keeps the shipped rows and device Favorite without injection', () => {
		const [tracks, queue] = surfaces();
		expect(tracks).toContain('data-testid="library-item"');
		expect(queue).toContain('data-testid="queue-item"');
		for (const body of [tracks, queue]) {
			expect(body).toContain('aria-label="Star Night Drive"');
			expect(body).not.toContain('aria-label="Like Night Drive"');
		}
	});

	it.each([
		['absent', undefined],
		['suspended', { ...readiness(), runIfCurrent: undefined, authoritative: false, authBoundary: true }],
		['stale', { ...readiness(), isCurrent: (): boolean => false }]
	] as const)('hides Likes and rejects a retained toggle with %s readiness', async (_case, next) => {
		const { owner, fetchFn, publish } = ownership();
		const controls = createTrackLikeControls(() => owner);
		const toggle = controls(track.id)!.toggle;
		publish(next);
		for (const body of surfaces(controls)) {
			expect(body).not.toContain('aria-label="Like Night Drive"');
			expect(body).toContain('aria-label="Star Night Drive"');
		}
		await toggle();
		expect(fetchFn).not.toHaveBeenCalled();
	});

	it('does not dispatch or report a network error when admission is refused', async () => {
		const { owner, fetchFn, publish } = ownership();
		publish(readiness(() => false));
		const controls = createTrackLikeControls(() => owner);
		await controls(track.id)!.toggle();
		expect(fetchFn).not.toHaveBeenCalled();
		expect(owner.failed).not.toHaveBeenCalled();
		expectLikeButtons(controls, false, false);
	});

	it('rechecks receipt identity after waiting for admission', async () => {
		const { owner, fetchFn, publish } = ownership();
		const admission = Promise.withResolvers<boolean>();
		publish(readiness(() => admission.promise));
		const controls = createTrackLikeControls(() => owner);
		const request = controls(track.id)!.toggle();
		publish(readiness());
		admission.resolve(true);
		await request;
		expect(fetchFn).not.toHaveBeenCalled();
		expectLikeButtons(controls, false, false);
	});

	it('shares pending state and publishes only after an acknowledged write', async () => {
		const { owner, fetchFn } = ownership();
		const reply = Promise.withResolvers<Response>();
		fetchFn.mockReturnValueOnce(reply.promise).mockResolvedValueOnce(Response.json({ target_id: track.id, liked: false }));
		const controls = createTrackLikeControls(() => owner);
		expect(controls('unknown')).toBeUndefined();
		expect(surfaces(controls, true)[1]).not.toContain('aria-label="Like Night Drive"');
		expectLikeButtons(controls, false, false);
		const staleToggle = controls(track.id)!.toggle;
		const request = staleToggle();
		await controls(track.id)!.toggle();
		expect(fetchFn).toHaveBeenCalledExactlyOnceWith(path, expect.objectContaining({
			method: 'PUT', headers: expect.objectContaining({ 'x-nntune-session-binding': owner.binding })
		}));
		expectLikeButtons(controls, false, true);
		reply.resolve(response());
		await request;
		expect(owner.confirm).toHaveBeenCalledExactlyOnceWith(track.id, track.id, true);
		expectLikeButtons(controls, true, false);
		await staleToggle();
		expect(fetchFn).toHaveBeenCalledTimes(1);
		await controls(track.id)!.toggle();
		expect(fetchFn).toHaveBeenNthCalledWith(2, path, expect.objectContaining({ method: 'DELETE' }));
		expectLikeButtons(controls, false, false);
	});

	it.each([
		['an invalid confirmation', () => Response.json({ target_id: 'other', liked_at: 1 })],
		['a rejected binding', () => Response.json({ error: 'session binding mismatch' }, { status: 412 })]
	] as const)('keeps confirmed state after %s and allows retry', async (_case, failure) => {
		const { owner, fetchFn } = ownership();
		fetchFn.mockResolvedValueOnce(failure()).mockResolvedValueOnce(response());
		const controls = createTrackLikeControls(() => owner);
		await controls(track.id)!.toggle();
		expect(owner.failed).toHaveBeenCalledExactlyOnceWith(track.id, expect.any(Error));
		expect(owner.confirm).not.toHaveBeenCalled();
		expectLikeButtons(controls, false, false);
		await controls(track.id)!.toggle();
		expectLikeButtons(controls, true, false);
	});

	it.each(['replaced receipt', 'refused publication', 'retired owner'] as const)(
		'drops delayed completion after %s', async (boundary) => {
			const a = ownership();
			const b = ownership();
			const reply = Promise.withResolvers<Response>();
			const dispatched = Promise.withResolvers<void>();
			a.fetchFn.mockImplementationOnce(() => { dispatched.resolve(); return reply.promise; });
			let admitted = true;
			a.publish(readiness(() => admitted));
			let owner = a.owner;
			const controls = createTrackLikeControls(() => owner);
			const request = controls(track.id)!.toggle();
			await dispatched.promise;
			if (boundary === 'replaced receipt') a.publish(readiness());
			if (boundary === 'refused publication') admitted = false;
			if (boundary === 'retired owner') owner = b.owner;
			if (boundary === 'retired owner') reply.reject(new Error('offline'));
			else reply.resolve(response());
			await request;
			expect(a.owner.confirm).not.toHaveBeenCalled();
			expect(a.owner.failed).not.toHaveBeenCalled();
			expect(b.fetchFn).not.toHaveBeenCalled();
			expectLikeButtons(controls, false, false);
		}
	);
});
