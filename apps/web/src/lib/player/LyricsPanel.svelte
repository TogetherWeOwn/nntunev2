<script module lang="ts">
	import type { ResolvedLyrics } from '#lib/server/lyrics.js';

	export type TrackLyrics = Readonly<{ trackId: string; result: ResolvedLyrics }>;
</script>

<script lang="ts">
	let { trackId, lyrics }: { trackId: string; lyrics: TrackLyrics | null } = $props();

	// Empty timed cues clear the sung line; they aren't lines in this static view.
	const lines = $derived(
		lyrics?.trackId === trackId ? lyrics.result.cues.filter(({ text }) => text.trim() !== '') : []
	);
</script>

{#if lines.length > 0}
	{#key trackId}
		<details>
			<summary>Lyrics</summary>
			<ol role="list">
				{#each lines as { text }}
					<li>{text}</li>
				{/each}
			</ol>
		</details>
	{/key}
{/if}

<style>
	details {
		max-inline-size: 65ch;
		margin-block-start: var(--nt-space-3);
	}
	summary {
		min-block-size: 3rem;
		align-content: center;
		cursor: pointer;
	}
	ol {
		list-style: none;
		padding: 0;
		margin: 0;
	}
	li {
		white-space: pre-wrap;
		overflow-wrap: anywhere;
	}
	li + li {
		margin-block-start: var(--nt-space-2);
	}
</style>
