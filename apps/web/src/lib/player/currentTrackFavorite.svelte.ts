import { displayTitle } from '../library/order.js';
import { currentTrack, type QueueState } from './queue.js';
import type { createQueueController } from './queueController.js';

export type FavoriteWrite = 'saved' | 'visit' | null;
type FavoriteOwner = { read(id: string): readonly string[] | null; write(id: string, pressed: boolean): FavoriteWrite };

type FavoriteIntent = { id: string; receipt: string; membership: readonly string[] };

/** The player borrows the mounted Library's membership; it never owns a copy. */
export function createCurrentTrackFavorite(queue: ReturnType<typeof createQueueController>) {
	let owner = $state.raw<{ member: FavoriteOwner; version: number } | null>(null);
	let ownerVersion = 0;
	let outcome = $state.raw<(FavoriteIntent & { text: string; version: number }) | null>(null);

	function bind(member: FavoriteOwner): () => void {
		const binding = { member, version: ++ownerVersion };
		owner = binding;
		outcome = null;
		return () => {
			if (owner !== binding) return;
			owner = null;
			outcome = null;
		};
	}

	function view(snapshot: QueueState) {
		const track = currentTrack(snapshot);
		const revision = queue.revision;
		if (!owner || !track || queue.purging || snapshot !== queue.snapshot || !queue.isCurrent(revision)) return null;
		const binding = owner;
		const membership = binding.member.read(track.id);
		if (!membership || binding !== owner || !queue.isCurrent(revision)) return null;
		return { id: track.id, title: displayTitle(track), pressed: membership.includes(track.id),
			receipt: `${binding.version}:${revision}`, membership };
	}

	const matches = (current: FavoriteIntent | null, intent: FavoriteIntent) =>
		current?.receipt === intent.receipt && current.id === intent.id && current.membership === intent.membership;

	function activate(intent: FavoriteIntent): void {
		const target = view(queue.snapshot);
		if (!owner || target === null || !matches(target, intent)) return;
		const pressed = !target.pressed;
		const result = owner.member.write(target.id, pressed);
		if (!result) return;
		// A retired selection or owner cannot publish a success message.
		const updated = view(queue.snapshot);
		if (!updated || updated.receipt !== intent.receipt || updated.pressed !== pressed) return;
		outcome = {
			...updated, version: (outcome?.version ?? 0) + 1,
			text: `${pressed ? 'Starred' : 'Unstarred'} ${target.title}${result === 'visit' ? ' for this visit' : ''}.`
		};
	}

	return {
		bind, view, activate,
		clearFeedback: () => { outcome = null; },
		feedback: (target: FavoriteIntent | null) => outcome && matches(target, outcome) ? outcome : null
	};
}
