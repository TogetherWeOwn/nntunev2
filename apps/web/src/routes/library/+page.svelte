<script lang="ts">
	// Library shell (TOG-25176). The hooks below are the e2e contract
	// (e2e/helpers/contract.ts TESTIDS); listing, streaming, metering, and
	// plan-limit enforcement land with the venture slices that own them
	// (ingest/stream/usage, e.g. TOG-23637 for the meter). Until then the
	// page renders the empty state honestly instead of faking tracks.
	interface Track {
		id: string;
		title: string;
	}

	const tracks: Track[] = [];
	const currentTrack: Track | null = null;
	const overLimit = false;
</script>

<svelte:head>
	<title>NNTune — your library</title>
	<meta name="description" content="Your NNTune music library." />
</svelte:head>

<main class="nt-wrap">
	<h1>Your library</h1>

	{#if overLimit}
		<p data-testid="plan-limit-notice" role="alert">
			You are over your plan limit — streaming is paused until the next cycle or an upgrade.
		</p>
	{/if}

	<section aria-label="Usage">
		<h2>Usage</h2>
		<p data-testid="usage-meter" role="status">
			Usage metering lands with the usage-meter slice — no streamed bytes recorded yet.
		</p>
	</section>

	<section aria-label="Tracks">
		<h2>Tracks</h2>
		{#if tracks.length === 0}
			<p>No tracks yet. Import an NZB to get started.</p>
			<p><a href="/library/import">Import</a></p>
		{:else}
			<ul>
				{#each tracks as track (track.id)}
					<li><button data-testid="library-item" type="button">{track.title}</button></li>
				{/each}
			</ul>
		{/if}
	</section>

	{#if currentTrack}
		<section aria-label="Player">
			<h2>Now playing</h2>
			<audio data-testid="audio-player" controls src=""></audio>
		</section>
	{/if}
</main>
