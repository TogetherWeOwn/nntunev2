import { PUBLIC_API_BASE_URL } from '$app/env/public';
import { SvelteMap } from 'svelte/reactivity';
import { ApiClient, ApiError } from '../api/client.js';
import type { AccountSession } from '../auth-fence.js';
import { isLibraryItem, type LibraryItem } from './cache.js';
import { loadLikes, MAX_LIKES, trackLikeTarget } from './likes.js';
import type { LibraryRefreshUpdate } from './refresh.js';
import { validBinding, verifyLibrarySession } from './session-verification.js';
import { createTrackLikeControls, type TrackLikeOwner } from './track-like-controls.js';
import { loadTrackTaste, type TrackTaste } from './taste.js';

function readPreferences(client: ApiClient, binding: string) {
	return {
		likes: loadLikes(client).catch(() => undefined),
		taste: loadTrackTaste(client, binding).catch(() => undefined)
	};
}

/** One owner per verified Library listing, shared by Tracks and Queue. */
export function createLibraryTrackLikes(onInvalidated?: () => void) {
	const retry = 'Account likes are unavailable. Refresh the library to try again.';
	let active: AbortController | undefined;
	type LibraryLikeOwner = TrackLikeOwner & { accountSession: AccountSession; targets: Map<string, string | undefined> };
	type RefreshRead = { controller: AbortController; result: Promise<ReturnType<typeof readPreferences> | undefined> };
	let owner = $state.raw<LibraryLikeOwner>();
	let refreshing: { owner: LibraryLikeOwner; confirmed: Map<string, TrackTaste | null>; failed: boolean; read?: RefreshRead } | undefined;
	let feedback = $state('');
	const writes = new Map<string, Set<Promise<void>>>();
	const controls = createTrackLikeControls(() => owner, (binding, settled) => {
		const pending = writes.get(binding) ?? new Set();
		writes.set(binding, pending);
		pending.add(settled);
		void settled.then(() => {
			pending.delete(settled);
			if (!pending.size) writes.delete(binding);
		});
	});

	function retire(keepRead?: AbortController) {
		active?.abort();
		if (refreshing?.read?.controller !== keepRead) refreshing?.read?.controller.abort();
		active = undefined;
		owner = undefined;
		refreshing = undefined;
	}

	function beginRefresh() {
		if (!owner) return retire();
		const overlap: NonNullable<typeof refreshing> = { owner, confirmed: new Map(), failed: false };
		refreshing = overlap;
		const { accountSession, binding } = owner;
		if (writes.has(binding)) return;
		const controller = new AbortController();
		const result = accountSession.run(() => {
			if (refreshing !== overlap || writes.has(binding)) return;
			// Only a confirmation after these reads start can be newer than their snapshot.
			overlap.confirmed.clear();
			const session = { owner: accountSession, binding };
			const client = new ApiClient({ baseUrl: PUBLIC_API_BASE_URL, signal: controller.signal, session });
			return readPreferences(client, binding);
		}).catch(() => undefined);
		overlap.read = { controller, result };
	}

	function invalidate() {
		if (!active && !feedback) return;
		retire();
		onInvalidated?.();
		feedback = retry;
	}

	async function publish(receipt: LibraryRefreshUpdate, items: readonly LibraryItem[]) {
		const overlap = refreshing;
		const { sessionBinding: binding, accountSession, runIfCurrent: run } = receipt;
		if (!validBinding(binding) || !accountSession?.isCurrent() || !run || !receipt.isCurrent() ||
			receipt.authoritative !== true || receipt.authBoundary !== false || !Array.isArray(items)) return retire();
		// A pending or failed write needs a new read after it settles. A different
		// account always gets its own read after the catalogue has been verified.
		const started = overlap?.owner.binding === binding && overlap.owner.accountSession === accountSession &&
			!overlap.failed && !writes.has(binding) ? overlap.read : undefined;
		retire(started?.controller);
		const { signal } = active = started?.controller ?? new AbortController();
		const session = { owner: accountSession, binding };
		const readClient = new ApiClient({ baseUrl: PUBLIC_API_BASE_URL, signal, session });
		const guarded = <T,>(work: () => T) => run(() => {
			if (!signal.aborted && accountSession.isCurrent() && receipt.isCurrent()) return work();
		});
		try {
			// Retire the old owner immediately, but let its already-dispatched
			// writes settle before reading the same account again.
			await Promise.all(writes.get(binding) ?? []);
			const early = await started?.result;
			const read = early ?? await guarded(() => readPreferences(readClient, binding));
			if (!read) return;
			const list = await read.likes;
			if (list?.binding !== binding) throw new Error('Likes session could not be verified');
			const saved = await read.taste;
			const verify = () => verifyLibrarySession(binding, signal, accountSession).catch(() => false);
			const check = await guarded(() => ({ response: verify() }));
			if (!check) return;
			if (!await check.response) throw new Error('Likes session could not be verified');
			await guarded(() => {
				const targets = new Map<string, string | undefined>();
				for (const item of items) {
					if (!isLibraryItem(item)) continue;
					const { id, like_target_id } = item;
					const target = trackLikeTarget(id, like_target_id);
					// Repeat occurrences can share a target. Conflicting resolutions disable this ID.
					targets.set(id, !targets.has(id) || targets.get(id) === target ? target : undefined);
				}
				const liked = new Set(list.items.filter((item) => item.kind === 'track').map((item) => item.targetId));
				// A full response cannot prove the absence of an older like.
				const unlisted = list.items.length < MAX_LIKES ? false : undefined;
				// Explicit taste rows and confirmed writes take precedence over the Likes snapshot.
				const preferences = new SvelteMap<string, TrackTaste | null>(saved?.tracks);
				if (early && overlap) {
					for (const target of targets.values()) {
						if (!target) continue;
						const confirmed = overlap.confirmed.get(target);
						if (confirmed !== undefined) preferences.set(target, confirmed);
					}
				}
				// A pending listing request does not revoke verified Like targets. Retiring
				// this owner still fences them on publication, failure, or auth change.
				const likeReceipt: LibraryRefreshUpdate = {
					...receipt,
					isCurrent: () => !signal.aborted && accountSession.isCurrent() &&
						(receipt.isCurrent() || refreshing?.owner === next),
					runIfCurrent: (work) => accountSession.run(() => likeReceipt.isCurrent() ? work() : undefined)
				};
				const next: LibraryLikeOwner = {
					accountSession, binding, client: new ApiClient({ baseUrl: PUBLIC_API_BASE_URL, session }), targets, readiness: () => likeReceipt,
					liked: (id) => preferences.has(id) ? preferences.get(id)?.signal === 'love' : liked.has(id) || unlisted,
					confirm: (id, confirmedId, value) => {
						if (id !== confirmedId) return invalidate();
						const preference: TrackTaste | null = value ? { signal: 'love', until: null } : null;
						preferences.set(id, preference);
						if (refreshing?.owner === next) refreshing.confirmed.set(id, preference);
						feedback = '';
					},
					failed: (_id, error, taste) => {
						if (refreshing?.owner === next) refreshing.failed = true;
						if (error instanceof ApiError && [401, 403, 412].includes(error.status)) invalidate();
						else feedback = `Could not confirm that ${taste ? 'track preference' : 'Like'} change. Try again or refresh the library.`;
					},
					taste: saved && {
						isCurrent: receipt.isCurrent,
						read: (id) => preferences.has(id) ? preferences.get(id) : saved.complete ? null : undefined,
						verify,
						invalidate,
						confirm: (id, value) => {
							preferences.set(id, value);
							feedback = value ? 'Track preference saved to your account.' : 'Track preference cleared.';
						}
					}
				};
				owner = next;
				feedback = '';
			});
		} catch {
			await guarded(() => { feedback = retry; }).catch(() => {});
		}
	}

	return { beginRefresh, publish, clear: () => { retire(); feedback = ''; }, invalidate, retire, get feedback() { return feedback; }, controls(id: string) {
		const target = owner?.targets.get(id);
		return target ? controls(target) : undefined;
	} };
}
