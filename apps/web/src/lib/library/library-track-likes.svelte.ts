import { PUBLIC_API_BASE_URL } from '$app/env/public';
import { SvelteMap } from 'svelte/reactivity';
import { ApiClient, ApiError } from '../api/client.js';
import type { LibraryItem } from './cache.js';
import { loadLikes, MAX_LIKES, trackLikeTarget } from './likes.js';
import type { LibraryRefreshUpdate } from './refresh.js';
import { verifyLibrarySession } from './session-verification.js';
import { createTrackLikeControls, type TrackLikeOwner } from './track-like-controls.js';
import { loadTrackTaste, type TrackTaste } from './taste.js';

/** One owner per verified Library listing, shared by Tracks and Queue. */
export function createLibraryTrackLikes(onInvalidated?: () => void) {
	const retry = 'Account likes are unavailable. Refresh the library to try again.';
	let active: AbortController | undefined;
	let owner = $state.raw<TrackLikeOwner & { targets: Map<string, string | undefined> }>();
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

	function retire() {
		active?.abort();
		active = undefined;
		owner = undefined;
	}

	function invalidate() {
		if (!active && !feedback) return;
		retire();
		onInvalidated?.();
		feedback = retry;
	}

	async function publish(receipt: LibraryRefreshUpdate, items: readonly LibraryItem[]) {
		retire();
		const { sessionBinding: binding, runIfCurrent: run } = receipt;
		if (!binding || !run || !receipt.isCurrent() || !receipt.authoritative || receipt.authBoundary) return;
		const { signal } = active = new AbortController();
		const readClient = new ApiClient({ baseUrl: PUBLIC_API_BASE_URL, signal });
		const guarded = <T,>(work: () => T) => run(() => {
			if (!signal.aborted && receipt.isCurrent()) return work();
		});
		try {
			// Retire the old owner immediately, but let its already-dispatched
			// writes settle before reading the same account again.
			await Promise.all(writes.get(binding) ?? []);
			const read = await guarded(() => ({
				likes: loadLikes(readClient).catch(() => undefined),
				taste: loadTrackTaste(readClient, binding).catch(() => undefined)
			}));
			if (!read) return;
			const list = await read.likes;
			if (list?.binding !== binding) throw new Error('Likes session could not be verified');
			const saved = await read.taste;
			const check = await guarded(() => ({ response: verifyLibrarySession(binding, signal).catch(() => false) }));
			if (!check) return;
			if (!await check.response) throw new Error('Likes session could not be verified');
			await guarded(() => {
				const targets = new Map(items.map(({ id, like_target_id }) => [id, trackLikeTarget(id, like_target_id)]));
				const liked = new Set(list.items.filter((item) => item.kind === 'track').map((item) => item.targetId));
				// A full response cannot prove the absence of an older like.
				const unlisted = list.items.length < MAX_LIKES ? false : undefined;
				// Explicit taste rows and confirmed writes take precedence over the Likes snapshot.
				const preferences = new SvelteMap<string, TrackTaste | null>(saved?.tracks);
				owner = {
					binding, client, targets, readiness: () => receipt,
					liked: (id) => preferences.has(id) ? preferences.get(id)?.signal === 'love' : liked.has(id) || unlisted,
					confirm: (id, confirmedId, value) => {
						if (id !== confirmedId) return invalidate();
						preferences.set(id, value ? { signal: 'love', until: null } : null);
						feedback = '';
					},
					failed: (_id, error, taste) => {
						if (error instanceof ApiError && [401, 403, 412].includes(error.status)) invalidate();
						else feedback = `Could not confirm that ${taste ? 'track preference' : 'Like'} change. Try again or refresh the library.`;
					},
					taste: saved && {
						read: (id) => preferences.has(id) ? preferences.get(id) : saved.complete ? null : undefined,
						verify: () => verifyLibrarySession(binding, signal),
						invalidate,
						confirm: (id, value) => {
							preferences.set(id, value);
							feedback = value ? 'Track preference saved to your account.' : 'Track preference cleared.';
						}
					}
				};
				feedback = '';
			});
		} catch {
			await guarded(() => { feedback = retry; }).catch(() => {});
		}
	}

	return { publish, clear: () => { retire(); feedback = ''; }, invalidate, retire, get feedback() { return feedback; }, controls(id: string) {
		const target = owner?.targets.get(id);
		return target ? controls(target) : undefined;
	} };
}
