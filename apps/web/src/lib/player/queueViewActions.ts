import type { PlayerSession } from './session.js';

/** The production queue view reports intent to its one persistent owner. */
export function queueViewActions(
	player: Pick<PlayerSession, 'selectNext' | 'shuffle' | 'editQueue' | 'playNext' | 'copyQueuedTrackLink' |
		'insertQueuedIds' | 'reorderQueuedIds' | 'playNextQueuedIds' | 'removeQueuedIds' | 'playQueuedTrack' | 'favoriteQueuedIds'>
) {
	return {
		oninsert: player.insertQueuedIds,
		onreorder: player.reorderQueuedIds,
		onplaynextids: player.playNextQueuedIds,
		onremoveids: player.removeQueuedIds,
		onplaynow: (id: string) => { void player.playQueuedTrack(id); },
		onfavoriteids: (ids: readonly string[]) => { player.favoriteQueuedIds(ids); },
		onnext: player.selectNext,
		onshuffle: player.shuffle,
		onmove: (from: number, to: number) => { player.editQueue({ type: 'move', from, to }); },
		onplaynext: player.playNext,
		onremove: (index: number) => { player.editQueue({ type: 'remove', index }); },
		onclear: () => { player.editQueue({ type: 'clear' }); },
		onselect: (index: number) => { player.editQueue({ type: 'select', index }); },
		oncopylink: player.copyQueuedTrackLink,
		onkeepcurrentandupcoming: () => { player.editQueue({ type: 'trimPrevious' }); },
		onclearupcoming: () => { player.editQueue({ type: 'trimUpcoming' }); }
	};
}
