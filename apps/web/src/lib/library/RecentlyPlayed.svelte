<script lang="ts">
	import { tick } from 'svelte';
	import type { LibraryItem } from './cache.js';
	import { displayArtist, displayTitle } from './order.js';
	import { formatRecentPlayedAt, type RecentEntry } from '../player/recentlyPlayed.js';

	let {
		entries,
		items,
		announcement,
		announcementVersion,
		liveRegions = true,
		onreplay,
		onforget,
		onclear
	}: {
		entries: RecentEntry[];
		items: LibraryItem[];
		announcement: string;
		announcementVersion: number;
		liveRegions?: boolean;
		onreplay: (id: string) => void;
		onforget: (id: string) => void;
		onclear: () => void;
	} = $props();

	let section: HTMLElement | undefined = $state();
	const rows = $derived(entries.map((entry) => ({
		...entry,
		track: items.find((item) => item.id === entry.trackId)
	})));

	async function forget(id: string): Promise<void> {
		const index = entries.findIndex((entry) => entry.trackId === id);
		onforget(id);
		await tick();
		// A removed row hands focus to its successor, then its predecessor,
		// then Clear. An unmounted section (auth/navigation) gets no late focus.
		if (!section?.isConnected) return;
		const survivors = section.querySelectorAll('[data-testid="library-recent-item"]');
		const target = survivors[Math.min(index, survivors.length - 1)]
			?.querySelector<HTMLButtonElement>('button:not(:disabled)') ??
			section.querySelector<HTMLButtonElement>('[data-testid="library-recently-played-clear"]');
		target?.focus();
	}
</script>

<section bind:this={section} class="nt-recents" data-testid="library-recently-played" aria-labelledby="library-recents-heading">
	<div class="nt-recents-header">
		<h2 id="library-recents-heading">Recently played</h2>
		<button class="nt-button" type="button" data-testid="library-recently-played-clear" onclick={onclear}>
			Clear all
		</button>
	</div>
	<p>Played in this tab this visit. Replay selects a track; use the player to start listening. Clearing the queue keeps this history.</p>
	{#if entries.length === 0}
		<p data-testid="library-recently-played-empty">Nothing played yet this visit</p>
	{:else}
		<ol>
			{#each rows as row (row.trackId)}
				<li data-testid="library-recent-item" data-track-id={row.trackId}>
					<div class="nt-recents-label">
						<span>{row.track ? displayTitle(row.track) : 'Unavailable track'}</span>
						{#if row.track && displayArtist(row.track)}
							<span class="nt-recents-detail">{displayArtist(row.track)}</span>
						{/if}
						<span class="nt-recents-detail" data-testid={`library-recent-played-at-${row.trackId}`}>
							{formatRecentPlayedAt(row.playedAt, Date.now())}
						</span>
					</div>
					<div class="nt-recents-actions">
						<button
							class="nt-button"
							type="button"
							data-testid={`library-recent-replay-${row.trackId}`}
							disabled={!row.track}
							aria-label={`Replay ${row.track ? displayTitle(row.track) : 'unavailable track'}`}
							onclick={() => onreplay(row.trackId)}
						>Replay</button>
						<button
							class="nt-button"
							type="button"
							data-testid={`library-recent-forget-${row.trackId}`}
							aria-label={`Forget ${row.track ? displayTitle(row.track) : 'unavailable track'}`}
							onclick={() => void forget(row.trackId)}
						>Forget</button>
					</div>
				</li>
			{/each}
		</ol>
	{/if}
	<p class="nt-recents-status" aria-live={liveRegions ? 'polite' : 'off'} aria-atomic="true">{#key announcementVersion}{announcement}{/key}</p>
</section>

<style>
	.nt-recents {
		margin-block: 1.5rem;
	}
	.nt-recents-header, li {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 0.75rem;
		flex-wrap: wrap;
	}
	.nt-recents-header h2 {
		margin: 0;
	}
	ol {
		list-style: none;
		padding: 0;
	}
	li {
		padding-block: 0.75rem;
		border-bottom: 1px solid var(--nt-border, currentColor);
	}
	.nt-recents-label {
		min-width: 0;
		flex: 1 1 12rem;
		overflow-wrap: anywhere;
	}
	.nt-recents-label span {
		display: block;
	}
	.nt-recents-detail {
		font-size: 0.875rem;
	}
	.nt-recents-actions {
		display: flex;
		gap: 0.5rem;
		flex-wrap: wrap;
	}
	button {
		min-height: 2.75rem;
		min-width: 2.75rem;
		flex-shrink: 0;
	}
	.nt-recents-status {
		min-height: 1.5em;
	}
</style>
