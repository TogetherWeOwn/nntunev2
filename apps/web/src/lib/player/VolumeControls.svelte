<script lang="ts">
	interface Props {
		volume: number;
		muted: boolean;
		restricted?: boolean;
		part?: 'controls' | 'status' | 'both';
		ready: boolean;
		/** Takes a 0–1 volume and returns the volume the owner observed. */
		onvolumeinput: (volume: number) => number;
		ontogglemute: () => void;
	}

	let { volume, muted, restricted = false, part = 'both', ready, onvolumeinput, ontogglemute }: Props = $props();
	const volumeId = $props.id();

	function handleInput(event: Event & { currentTarget: HTMLInputElement }): void {
		const input = event.currentTarget;
		const observed = onvolumeinput(Number(input.value) / 100);
		// Rejected writes can leave the prop unchanged but the native thumb moved.
		input.value = String(observed * 100);
	}
</script>

{#if part !== 'status'}
	<div class="nt-sound">
		<label for={volumeId}>
			Volume {Math.round(volume * 100)}%
			<input
				id={volumeId}
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
{/if}
{#if part !== 'controls'}<p role="status" hidden={!restricted}>Use system volume controls.</p>{/if}

<style>
	@media (pointer: coarse) {
		.nt-sound :is(input, button) {
			min-block-size: 44px;
			min-inline-size: 44px;
		}
		.nt-sound input {
			vertical-align: middle;
		}
	}
</style>
