import { PUBLIC_API_BASE_URL } from '$app/env/public';
import { SvelteMap } from 'svelte/reactivity';
import { ApiClient, ApiError } from '../api/client.js';
import type { LibraryItem } from './cache.js';
import { loadLikes, MAX_LIKES } from './likes.js';
import type { LibraryRefreshUpdate } from './refresh.js';
import { verifyLibrarySession } from './session-verification.js';
import { createTrackLikeControls, type TrackLikeOwner } from './track-like-controls.js';

function refused(error: unknown) {
	return error instanceof ApiError && [401, 403, 412].includes(error.status);
}

/** One owner per verified Library listing, shared by Tracks and Queue. */
export function createLibraryTrackLikes() {
	let active: AbortController | undefined;
	let owner = $state.raw<TrackLikeOwner>();
	let feedback = $state('');
	const writes = new Map<string, Set<Promise<unknown>>>();
	const client = new ApiClient({ baseUrl: PUBLIC_API_BASE_URL });
	const controls = createTrackLikeControls(() => owner, (binding, result) => {
		const pending = writes.get(binding) ?? new Set();
		writes.set(binding, pending);
		pending.add(result);
		void result.then(() => {
			pending.delete(result);
			if (!pending.size) writes.delete(binding);
		});
	});

	function clear() {
		active?.abort();
		active = undefined;
		owner = undefined;
		feedback = '';
	}

	async function publish(receipt: LibraryRefreshUpdate, items: readonly LibraryItem[]) {
		clear();
		const { sessionBinding: binding, runIfCurrent: run } = receipt;
		if (!binding || !run || !receipt.isCurrent() || !receipt.authoritative || receipt.authBoundary) return;
		const { signal } = active = new AbortController();
		const readClient = new ApiClient({ baseUrl: PUBLIC_API_BASE_URL, signal });
		const guarded = <T,>(work: () => T) => run(() => {
			if (!signal.aborted && receipt.isCurrent()) return work();
		});
		const retry = 'Account likes are unavailable. Refresh the library to try again.';
		try {
			// Retire the old owner immediately, but let its already-dispatched
			// writes settle before reading the same account again.
			await Promise.all(writes.get(binding) ?? []);
			const read = await guarded(() => ({ response: loadLikes(readClient).then(
				(value) => ({ value }), (error: unknown) => ({ error })
			) }));
			if (!read) return;
			const result = await read.response;
			if ('error' in result) throw result.error;
			const list = result.value;
			if (list.binding !== binding) throw new Error('Likes session changed');
			const check = await guarded(() => ({ response: verifyLibrarySession(binding, signal).catch(() => false) }));
			if (!check) return;
			if (!await check.response) throw new Error('Likes session could not be verified');
			await guarded(() => {
				const members = new Set(items.map((item) => item.id));
				const liked = new SvelteMap<string, boolean>();
				for (const item of list.items) if (item.kind === 'track') liked.set(item.targetId, true);
				// A full response cannot prove the absence of an older like.
				const unlisted = list.items.length < MAX_LIKES ? false : undefined;
				owner = {
					binding, client, readiness: () => receipt,
					liked: (id) => members.has(id) ? liked.get(id) ?? unlisted : undefined,
					confirm: (id, confirmedId, value) => {
						liked.set(id, value);
						liked.set(confirmedId, value);
						feedback = '';
					},
					failed: (_id, error) => {
						if (refused(error)) owner = undefined;
						feedback = refused(error) ? retry : 'Could not confirm that Like change. Try again or refresh the library.';
					}
				};
			});
		} catch {
			await guarded(() => { feedback = retry; }).catch(() => {});
		}
	}

	return { controls, publish, clear, get feedback() { return feedback; } };
}
