import { displayTitle } from '../library/order.js';
import { currentTrack, type QueueState } from './queue.js';
import type { createQueueController } from './queueController.js';

export type FavoriteWrite = 'saved' | 'visit' | null;
type Membership = { pressed: boolean; edit: number };
type FavoriteOwner = { read(id: string): Membership | null; write(id: string, pressed: boolean): Promise<FavoriteWrite> };
type Selection = Membership & { id: string; receipt: string };

/** The player borrows the mounted Library's membership; it never owns a copy. */
export function createCurrentTrackFavorite(queue: ReturnType<typeof createQueueController>) {
	let owner = $state.raw<{ member: FavoriteOwner; version: number } | null>(null);
	let ownerVersion = 0;
	let outcome = $state.raw<(Selection & { text: string }) | null>(null);

	function bind(member: FavoriteOwner): () => void {
		const binding = owner = { member, version: ++ownerVersion };
		outcome = null;
		return () => { if (owner === binding) { owner = null; outcome = null; } };
	}

	function view(snapshot: QueueState) {
		const track = currentTrack(snapshot);
		const revision = queue.revision;
		if (!owner || !track || queue.purging || snapshot !== queue.snapshot || !queue.isCurrent(revision)) return null;
		const binding = owner;
		const membership = binding.member.read(track.id);
		if (!membership || binding !== owner || !queue.isCurrent(revision)) return null;
		return { pressed: membership.pressed, edit: membership.edit,
			id: track.id, title: displayTitle(track), receipt: `${binding.version}:${revision}` };
	}

	const matches = (current: Selection | null, previous: Selection) => current?.receipt === previous.receipt &&
		current.id === previous.id && current.edit === previous.edit && current.pressed === previous.pressed;
	async function activate(intent: Selection): Promise<void> {
		const target = view(queue.snapshot);
		if (!owner || !target || !matches(target, intent)) return;
		const pressed = !target.pressed;
		const pending = owner.member.write(target.id, pressed);
		// The owner publishes the optimistic membership before awaiting the scoped lock.
		const applied = view(queue.snapshot);
		const result = await pending;
		if (!result || !applied || applied.receipt !== intent.receipt || applied.id !== intent.id ||
			applied.pressed !== pressed || !matches(view(queue.snapshot), applied)) return;
		outcome = { ...applied, text: `${pressed ? 'Starred' : 'Unstarred'} ${target.title}${result === 'visit' ? ' for this visit' : ''}.` };
	}

	return {
		bind, view, activate,
		clearFeedback: () => { outcome = null; },
		feedback: (target: Selection | null) => outcome && matches(target, outcome) ? outcome : null
	};
}
