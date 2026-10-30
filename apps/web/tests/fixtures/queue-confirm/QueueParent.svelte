<script lang="ts">
	import PlayerQueue from '../../../src/lib/player/PlayerQueue.svelte';

	// Test-only reusable host: record intents, never implement a queue transition.
	const queueValue = {
		tracks: [
			{ id: 'host-current', title: 'Host Current', artist: 'Host Artist', streamUrl: '', artworkUrl: null },
			{ id: 'host-next', title: 'Host Next', artist: 'Host Artist', streamUrl: '', artworkUrl: null }
		],
		currentIndex: 0
	};
	queueValue.tracks.forEach(Object.freeze);
	Object.freeze(queueValue.tracks);
	Object.freeze(queueValue);
	let calls: string[] = [];
	let clearAll = $state(() => { calls.push('clear-all'); });
	let clearUpcoming = $state<(() => void) | undefined>(() => { calls.push('original-trim'); });

	export function supply(mode: 'original' | 'none' | 'replacement') {
		clearUpcoming = mode === 'none' ? undefined : () => { calls.push(`${mode}-trim`); };
	}

	export function replaceClear() {
		clearAll = () => { calls.push('replacement-all'); };
	}

	export function inspect() {
		return { calls: [...calls], tracks: queueValue.tracks.map((track) => track.id), currentIndex: queueValue.currentIndex };
	}
</script>

<PlayerQueue
	state={queueValue}
	confirmDestructive={true}
	onclearupcoming={clearUpcoming}
	onkeepcurrentandupcoming={() => { calls.push('keep'); }}
	onnext={() => { calls.push('next'); }}
	onshuffle={() => { calls.push('shuffle'); }}
	onmove={() => { calls.push('move'); }}
	onplaynext={() => { calls.push('play-next'); }}
	onremove={() => { calls.push('remove'); }}
	onclear={clearAll}
/>
