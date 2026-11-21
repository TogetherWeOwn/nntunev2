<script lang="ts">
	type Props = { value: string | null; feedback: string; onchange: (control: HTMLSelectElement) => void };
	let { value, feedback, onchange }: Props = $props();
	const presets = ['0.5', '0.75', '1', '1.25', '1.5', '1.75', '2'];
	let attempt = $state(0);
</script>
<div>
	<label for="library-playback-speed">Playback speed</label>
	<select id="library-playback-speed" data-testid="library-playback-speed" value={value ?? ''} disabled={value === null}
		onchange={(event) => { onchange(event.currentTarget); attempt += 1; }}>
		{#if value === null || !presets.includes(value)}
			<option value={value ?? ''}>{value === null ? 'Unavailable' : `${value}x`}</option>
		{/if}
		{#each presets as preset}
			<option value={preset}>{preset}x</option>
		{/each}
	</select>
	<p data-testid="library-speed-status" role="status" aria-atomic="true">{#key attempt}{feedback}{/key}</p>
</div>
<style>
	p:empty { margin: 0; }
</style>
