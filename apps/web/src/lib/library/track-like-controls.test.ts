import { render } from 'svelte/server';
import { describe, expect, it, vi } from 'vitest';
import TrackRows from '../../routes/library/TrackRows.svelte';
import { ApiClient } from '../api/client.js';
import PlayerQueue from '../player/PlayerQueue.svelte';
import { createQueue } from '../player/queue.js';
import { createTrackLikeControls, type TrackLikeControls } from './track-like-controls.js';

const track = { id: 'Job_42', title: 'Night Drive', artist: '', streamUrl: '' };
const baseUrl = 'https://api.example.test';
const path = `${baseUrl}/v1/me/likes/track/${track.id}`;
const response = () => Response.json({ target_id: track.id, liked_at: 1 });

function ownership(initial = false) {
	let liked = initial;
	const fetchFn = vi.fn<typeof fetch>();
	return {
		fetchFn,
		owner: {
			client: new ApiClient({ baseUrl, fetchFn }),
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
		expect(body).not.toContain('aria-label="Star Night Drive"');
	}
}

describe('injected track and queue Like controls', () => {
	it('keeps the existing Stars without injection and hides Likes until the track is known', () => {
		for (const body of surfaces()) expect(body).toContain('aria-label="Star Night Drive"');
		const { owner, fetchFn } = ownership();
		let current: typeof owner | undefined;
		const controls = createTrackLikeControls(() => current);
		for (const body of surfaces(controls)) expect(body).not.toContain('aria-label="Like Night Drive"');
		current = owner;
		expect(controls('unknown')).toBeUndefined();
		expect(surfaces(controls, true)[1]).not.toContain('aria-label="Like Night Drive"');
		expect(fetchFn).not.toHaveBeenCalled();
	});

	it('shares pending state and changes both buttons only after an acknowledged write', async () => {
		const { owner, fetchFn } = ownership();
		const receipt = Promise.withResolvers<Response>();
		fetchFn.mockReturnValueOnce(receipt.promise).mockResolvedValueOnce(Response.json({ target_id: track.id, liked: false }));
		const controls = createTrackLikeControls(() => owner);
		expectLikeButtons(controls, false, false);
		const staleToggle = controls(track.id)!.toggle;
		const request = staleToggle();
		await controls(track.id)!.toggle();
		expect(fetchFn).toHaveBeenCalledExactlyOnceWith(path, expect.objectContaining({ method: 'PUT' }));
		expectLikeButtons(controls, false, true);
		receipt.resolve(response());
		await request;
		expect(owner.confirm).toHaveBeenCalledExactlyOnceWith(track.id, track.id, true);
		expectLikeButtons(controls, true, false);
		await staleToggle();
		expect(fetchFn).toHaveBeenCalledTimes(1);
		await controls(track.id)!.toggle();
		expect(fetchFn).toHaveBeenNthCalledWith(2, path, expect.objectContaining({ method: 'DELETE' }));
		expectLikeButtons(controls, false, false);
	});

	it('keeps confirmed state on an invalid receipt and allows retry', async () => {
		const { owner, fetchFn } = ownership();
		fetchFn.mockResolvedValueOnce(Response.json({ target_id: 'other', liked_at: 1 })).mockResolvedValueOnce(response());
		const controls = createTrackLikeControls(() => owner);
		await controls(track.id)!.toggle();
		expect(owner.failed).toHaveBeenCalledExactlyOnceWith(track.id);
		expect(owner.confirm).not.toHaveBeenCalled();
		expectLikeButtons(controls, false, false);
		await controls(track.id)!.toggle();
		expectLikeButtons(controls, true, false);
	});

	it.each(['success', 'failure'])('ignores late %s from a retired owner', async (outcome) => {
		const a = ownership();
		const b = ownership();
		const oldReceipt = Promise.withResolvers<Response>();
		const newReceipt = Promise.withResolvers<Response>();
		a.fetchFn.mockReturnValueOnce(oldReceipt.promise);
		b.fetchFn.mockReturnValueOnce(newReceipt.promise);
		let owner = a.owner;
		const controls = createTrackLikeControls(() => owner);
		const oldToggle = controls(track.id)!.toggle;
		const oldWrite = oldToggle();
		owner = b.owner;
		await oldToggle();
		expect(a.fetchFn).toHaveBeenCalledTimes(1);
		expect(b.fetchFn).not.toHaveBeenCalled();
		const newWrite = controls(track.id)!.toggle();
		if (outcome === 'success') oldReceipt.resolve(response());
		else oldReceipt.reject(new Error('offline'));
		await oldWrite;
		expect(a.owner.confirm).not.toHaveBeenCalled();
		expect(a.owner.failed).not.toHaveBeenCalled();
		expectLikeButtons(controls, false, true);
		newReceipt.resolve(response());
		await newWrite;
		expect(b.owner.confirm).toHaveBeenCalledExactlyOnceWith(track.id, track.id, true);
	});
});
