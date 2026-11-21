import { displayTitle } from '../library/order.js';
import { currentTrack, type QueueState } from './queue.js';
import type { createQueueController } from './queueController.js';

export type FavoriteWrite = 'saved' | 'visit' | null;
type FavoriteOwner = { read(id: string): readonly string[] | null; write(id: string, pressed: boolean): Promise<FavoriteWrite> };
type Selection = { id: string; receipt: string; pressed: boolean };

/** The player borrows the mounted Library's membership; it never owns a copy. */
export function createCurrentTrackFavorite(queue: ReturnType<typeof createQueueController>) {
	let owner = $state.raw<{ member: FavoriteOwner; version: number } | null>(null);
	let ownerVersion = 0, actionVersion = 0;
	let outcome = $state.raw<(Selection & { text: string; version: number }) | null>(null);

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

	const sameTrack = (current: Selection | null, previous: Selection) =>
		current?.receipt === previous.receipt && current.id === previous.id;

	async function activate(intent: Selection & { membership: readonly string[] }): Promise<void> {
		const target = view(queue.snapshot);
		if (!owner || !target || !sameTrack(target, intent) || target.membership !== intent.membership) return;
		const pressed = !target.pressed;
		const version = ++actionVersion;
		const result = await owner.member.write(target.id, pressed);
		const updated = view(queue.snapshot);
		if (!result || version !== actionVersion || !sameTrack(updated, intent) || updated?.pressed !== pressed) return;
		outcome = {
			id: intent.id, receipt: intent.receipt, pressed, version,
			text: `${pressed ? 'Starred' : 'Unstarred'} ${target.title}${result === 'visit' ? ' for this visit' : ''}.`
		};
	}

	return {
		bind, view, activate,
		clearFeedback: () => { outcome = null; },
		feedback: (target: Selection | null) => outcome && sameTrack(target, outcome) && target?.pressed === outcome.pressed ? outcome : null
	};
}
