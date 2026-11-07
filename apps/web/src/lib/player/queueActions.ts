import {
	clearQueue, clearUpcoming, keepCurrentAndUpcoming, moveTrack, removeTrack, selectTrack,
	type QueueState
} from './queue.js';

export type QueueAction =
	| { type: 'move'; from: number; to: number }
	| { type: 'remove'; index: number }
	| { type: 'select'; index: number }
	| { type: 'clear' | 'trimPrevious' | 'trimUpcoming' };

/** Metadata-only edits with the existing queue feedback; no-op gestures stay silent. */
export function applyQueueAction(state: QueueState, action: QueueAction): {
	queue: QueueState; feedback: string; repeat: boolean
} | null {
	let queue: QueueState;
	let feedback: string;
	let repeat = false;
	switch (action.type) {
		case 'move': {
			queue = moveTrack(state, action.from, action.to);
			if (queue === state) return null;
			feedback = `${state.tracks[action.from].title} moved to ${action.to + 1} of ${queue.tracks.length}`;
			break;
		}
		case 'remove': {
			queue = removeTrack(state, action.index);
			if (queue === state) return null;
			const track = state.tracks[action.index];
			feedback = `${track.title?.trim() ? track.title : track.id} removed from the queue.`;
			repeat = true;
			break;
		}
		case 'select': {
			if (action.index === state.currentIndex) return null;
			queue = selectTrack(state, action.index);
			if (queue === state) return null;
			const track = state.tracks[action.index];
			feedback = `Selected ${track.title?.trim() ? track.title : track.id}`;
			break;
		}
		case 'clear':
			queue = clearQueue(state);
			if (queue === state) return null;
			feedback = 'Queue cleared.';
			repeat = true;
			break;
		case 'trimPrevious': {
			queue = keepCurrentAndUpcoming(state);
			if (queue === state) return null;
			const removed = state.tracks.length - queue.tracks.length;
			feedback = `Removed ${removed} ${removed === 1 ? 'track' : 'tracks'} before the current track.`;
			break;
		}
		case 'trimUpcoming': {
			queue = clearUpcoming(state);
			if (queue === state) return null;
			const removed = state.tracks.length - queue.tracks.length;
			feedback = `Removed ${removed} upcoming ${removed === 1 ? 'track' : 'tracks'}.`;
			break;
		}
	}
	return { queue, feedback, repeat };
}
