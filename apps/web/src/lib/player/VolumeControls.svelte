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
	const validVolume = (value: number) => Number.isFinite(value) && value >= 0 && value <= 1;
	const canAdjust = $derived(ready && !restricted && validVolume(volume));
	const displayedVolume = $derived(validVolume(volume) ? volume * 100 : 0);

	function handleInput(event: Event & { currentTarget: HTMLInputElement }): void {
		const input = event.currentTarget;
		const requested = input.valueAsNumber / 100;
		const observed = canAdjust && validVolume(requested) ? onvolumeinput(requested) : NaN;
		// Rejected writes can leave the prop unchanged but the native thumb moved.
		input.valueAsNumber = validVolume(observed) ? observed * 100 : displayedVolume;
	}
</script>

{#if part !== 'status'}
	<div class="nt-sound">
		<label for={volumeId}>
			Volume {Math.round(displayedVolume)}%
			<input
				id={volumeId}
				type="range"
				min="0"
				max="100"
				value={displayedVolume}
				disabled={!canAdjust}
				oninput={handleInput}
			/>
		</label>
		<button type="button" disabled={!ready} aria-pressed={muted} onclick={() => { if (ready) ontogglemute(); }}>Mute</button>
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
