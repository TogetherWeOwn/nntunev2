import { displayTitle } from '../library/order.js';
import { currentTrack, type QueueState } from './queue.js';
import type { createQueueController } from './queueController.js';
export type FavoriteWrite = 'saved' | 'visit' | null;
type Membership = { pressed: boolean; edit: number; loading: boolean };
type FavoriteOwner = { read(id: string): Membership | null; write(id: string): Promise<FavoriteWrite> };
type Selection = Membership & { id: string; receipt: string };
/** The player borrows the mounted Library's membership; it never owns a copy. */
export function createCurrentTrackFavorite(queue: ReturnType<typeof createQueueController>) {
	let owner = $state.raw<{ member: FavoriteOwner; version: number } | null>(null), ownerVersion = 0;
	let outcome = $state.raw<(Selection & { text: string }) | null>(null);
	function bind(member: FavoriteOwner): () => void {
		const binding = owner = { member, version: ++ownerVersion };
		return () => { if (owner === binding) { owner = null; outcome = null; } };
	}
	function view(snapshot: QueueState) {
		const binding = owner, generation = queue.selectionGeneration, revision = queue.revision;
		const isCurrent = () => binding === owner && !queue.purging && snapshot === queue.snapshot &&
			queue.selectionGeneration === generation && queue.isCurrent(revision);
		if (!binding || !isCurrent()) return null;
		const track = currentTrack(snapshot);
		if (!track) return null;
		const membership = binding.member.read(track.id);
		if (!membership || !isCurrent()) return null;
		return { ...membership, id: track.id, title: displayTitle(track), receipt: `${binding.version}:${generation}` };
	}
	const matches = (current: Selection | null, previous: Selection) => current?.receipt === previous.receipt &&
		current.id === previous.id && current.edit === previous.edit && current.pressed === previous.pressed;
	async function activate(intent: Selection): Promise<void> {
		const target = view(queue.snapshot);
		if (!owner || !target || target.loading || !matches(target, intent)) return;
		const pressed = !target.pressed;
		try {
			const pending = owner.member.write(target.id);
			// The owner publishes the optimistic membership before awaiting the scoped lock.
			const applied = view(queue.snapshot);
			const result = await pending, completed = view(queue.snapshot);
			if ((result !== 'saved' && result !== 'visit') || !applied || !completed ||
				applied.receipt !== intent.receipt || applied.id !== intent.id ||
				applied.pressed !== pressed || !matches(completed, applied)) return;
			outcome = { ...completed, text: `${pressed ? 'Starred' : 'Unstarred'} ${target.title}${result === 'visit' ? ' for this visit' : ''}.` };
		} catch { /* A failed owner write has no success announcement. */ }
	}
	return {
		bind, view, activate,
		reconcileFeedback(target: Selection | null) {
			if (outcome && !matches(target, outcome)) outcome = null;
			else if (outcome?.loading && target && !target.loading) outcome = { ...outcome, loading: false };
		},
		feedback: (target: Selection | null) => outcome && !outcome.loading && matches(target, outcome) ? outcome : null
	};
}
