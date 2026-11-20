<script lang="ts">
	interface Props {
		volume: number;
		muted: boolean;
		restricted?: boolean;
		ready: boolean;
		/** Takes a 0–1 volume and returns the volume the owner observed. */
		onvolumeinput: (volume: number) => number;
		ontogglemute: () => void;
	}

	let { volume, muted, restricted = false, ready, onvolumeinput, ontogglemute }: Props = $props();

	function handleInput(event: Event & { currentTarget: HTMLInputElement }): void {
		const input = event.currentTarget;
		const observed = onvolumeinput(Number(input.value) / 100);
		// Rejected writes can leave the prop unchanged but the native thumb moved.
		input.value = String(observed * 100);
	}
</script>

<div class="nt-sound">
	<label>
		Volume {Math.round(volume * 100)}%
		<input
			type="range"
			min="0"
			max="100"
			value={volume * 100}
			disabled={!ready || restricted}
			oninput={handleInput}
		/>
	</label>
	<button type="button" aria-pressed={muted} onclick={ontogglemute}>Mute</button>
</div>
<p role="status" hidden={!restricted}>Use system volume controls.</p>
