<script lang="ts">
	interface Props {
		enabled: boolean;
		/** Called with the new state when the toggle is activated. */
		onchange?: (enabled: boolean) => void;
	}

	let { enabled, onchange }: Props = $props();
</script>

<!--
	ReplayGain loudness-leveling toggle. A native button, so it is keyboard
	reachable by construction; `aria-pressed` announces the on/off state to
	assistive tech. Persistence lives with the host page, which threads
	`loadLevelingEnabled` / `saveLevelingEnabled` through these props.
-->
<button
	type="button"
	data-testid="leveling-toggle"
	aria-pressed={enabled}
	aria-label="Loudness leveling"
	onclick={() => onchange?.(!enabled)}
>
	Leveling: {enabled ? 'On' : 'Off'}
</button>
