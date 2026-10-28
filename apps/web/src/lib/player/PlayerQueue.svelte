<script lang="ts">
	import { tick } from 'svelte';
	import { currentTrack, type QueueState, type Track } from './queue.js';
	import { matchesQueueQuery } from './queueFilter.js';
	import ShuffleUpcoming from './ShuffleUpcoming.svelte';

	interface Props {
		state: QueueState;
		/** Transport stays with the host page: called when Next is activated. */
		onnext?: () => void;
		/** Queue-only edit: the host must not restart playback. */
		onshuffle?: () => void;
		radioActive?: boolean;
		/** Queue management stays with the host page too: the panel reports intent, never mutates. */
		onmove?: (from: number, to: number) => void;
		onplaynext?: (index: number) => void;
		onremove?: (index: number) => void;
		onclear?: () => void;
		/** Favorites stay with the library host; other queues omit the intent. */
		favoriteIds?: ReadonlySet<string>;
		ontogglefavorite?: (id: string) => void;
		/**
		 * Metadata-only direct selection: the host applies the validated
		 * selectTrack transition and owns the announcement; the panel reports
		 * the ORIGINAL queue index once and never mutates, transports or
		 * fetches. Each Select control renders only when provided.
		 */
		onselect?: (index: number) => void;
		/**
		 * Share intent by stable track id: the host builds the link, owns the
		 * clipboard and the polite outcome. The panel only reports and never
		 * mutates, selects or plays. The control renders only when provided.
		 */
		oncopylink?: (id: string) => void;
		/**
		 * Trim intents, same contract: the host applies the transition and
		 * owns the announcement; the panel reports once and never mutates.
		 * Each control renders only when its callback is provided.
		 */
		onkeepcurrentandupcoming?: () => void;
		onclearupcoming?: () => void;
		/** Polite queue-action announcements, owned by the host; the panel only renders the region. */
		announcement?: string;
		/** Real action identity: equal outcome text must still update the live region. */
		announcementVersion?: number;
		/**
		 * Transient queue-only filter text. Bindable so a host that
		 * unmounts/remounts the panel (library refresh swaps branches)
		 * can keep the query across mounts; unbound parents get the same
		 * component-local default. Never persisted or fetched.
		 */
		filterQuery?: string;
		/**
		 * Whether the panel's polite regions are exposed as live statuses.
		 * The library host suspends them while a library refresh is in
		 * flight so the loading transient keeps a single page-level
		 * status; content, controls and focus are untouched. Defaults to
		 * exposed; component harnesses leave it on.
		 */
		liveRegions?: boolean;
		/**
		 * Opt-in two-step guard for the two destructive intents (Clear queue,
		 * Clear upcoming): the panel opens an inline Cancel/Confirm prompt and
		 * reports the intent only after Confirm. Hosts that omit it keep the
		 * one-step behavior.
		 */
		confirmDestructive?: boolean;
	}

	// Renamed locally: a `state` binding would shadow the `$state` rune.
	let {
		state: queue,
		onnext,
		onshuffle,
		radioActive = false,
		onmove,
		onplaynext,
		onremove,
		onclear,
		onselect,
		oncopylink,
		favoriteIds = new Set<string>(),
		ontogglefavorite,
		onkeepcurrentandupcoming,
		onclearupcoming,
		announcement = '',
		announcementVersion = 0,
		filterQuery = $bindable(''),
		liveRegions = true,
		confirmDestructive = false
	}: Props = $props();

	const confirmId = $props.id();

	let current = $derived(currentTrack(queue));

	/**
	 * Defensive selected slot: only an in-range integer index counts as a
	 * selection. Anything else (empty queue, negative, past-end, fractional,
	 * NaN, infinite) renders the unselected summary and no current row —
	 * never a clamped or fabricated position.
	 */
	let selectedIndex = $derived(
		Number.isInteger(queue.currentIndex) &&
			queue.currentIndex >= 0 &&
			queue.currentIndex < queue.tracks.length
			? queue.currentIndex
			: -1
	);

	let trackCount = $derived(queue.tracks.length);
	let upcoming = $derived(selectedIndex < 0 ? 0 : trackCount - selectedIndex - 1);

	let list: HTMLOListElement | null = $state(null);
	let emptyTarget: HTMLElement | null = $state(null);
	let filterEmptyTarget: HTMLElement | null = $state(null);
	let revealCurrentButtonEl: HTMLButtonElement | null = $state(null);

	function currentRowElement(): HTMLElement | null {
		return (
			list?.querySelector<HTMLElement>('[data-testid="queue-item"][aria-current="true"]') ?? null
		);
	}

	/**
	 * Stable-id survivor for remove recovery: the first id after the removed
	 * one in the pre-action displayed order that still exists, otherwise the
	 * nearest preceding one. Re-resolved against the live DOM after render,
	 * never from stale array indices.
	 */
	function survivorId(
		beforeIds: string[],
		removedId: string | undefined,
		current: Set<string>
	): string | null {
		const at = removedId === undefined ? -1 : beforeIds.indexOf(removedId);
		for (let i = at + 1; i < beforeIds.length; i++) {
			const id = beforeIds[i]!;
			if (current.has(id)) return id;
		}
		for (let i = Math.min(at - 1, beforeIds.length - 1); i >= 0; i--) {
			const id = beforeIds[i]!;
			if (current.has(id)) return id;
		}
		return null;
	}

	function focusRemoveFor(trackId: string): boolean {
		const rows = list?.querySelectorAll('[data-testid="queue-item"]') ?? [];
		for (const row of rows) {
			if (row.getAttribute('data-track-id') !== trackId) continue;
			const remove = row.querySelector<HTMLButtonElement>('button[aria-label^="Remove "]');
			if (!remove) return false;
			remove.focus({ preventScroll: true });
			remove.scrollIntoView({ block: 'nearest', behavior: 'instant' });
			return true;
		}
		return false;
	}

	function focusEmptyTarget(): void {
		if (!emptyTarget) return;
		emptyTarget.focus({ preventScroll: true });
		emptyTarget.scrollIntoView({ block: 'nearest', behavior: 'instant' });
	}

	/**
	 * Zero-visible fallback: tracks survive behind the filter but no row
	 * renders, so the no-matches status — not the "Queue is empty" target,
	 * which would misannounce — receives recovery focus.
	 */
	function focusFilterEmptyTarget(): boolean {
		if (!filterEmptyTarget) return false;
		filterEmptyTarget.focus({ preventScroll: true });
		filterEmptyTarget.scrollIntoView({ block: 'nearest', behavior: 'instant' });
		return true;
	}

	/**
	 * Focus is only recovered when this explicit action stranded it: the
	 * activated control held focus at dispatch and, after render, focus sits
	 * on nothing (body/null) — or is still on that same control while the
	 * action just disabled it. DOM removal strands synchronously, but a
	 * control that becomes disabled keeps focus through the flush microtask
	 * and the browser drops it to the body on the next rendering steps, so
	 * waiting for body/null alone would miss Clear every time. Any focus
	 * that has moved on to a different live element — Tab movement, pointer
	 * flow, host-driven restore — is left alone, and prop-only updates
	 * (refresh, auth purge) never reach here.
	 */
	function focusStranded(heldFocus: boolean, button: EventTarget | null): boolean {
		if (!heldFocus) return false;
		const active = document.activeElement;
		if (active === null || active === document.body) return true;
		return (
			button instanceof HTMLButtonElement && button.disabled && active === button
		);
	}

	/**
	 * Report one remove intent, then land focus deterministically: the next
	 * surviving displayed row's Remove, otherwise the previous, otherwise
	 * the no-matches status when hidden tracks survive behind the filter,
	 * otherwise the stable empty target. Consecutive keyboard removals stay
	 * operable because each recovery lands on a live Remove control.
	 */
	async function reportRemove(event: Event, index: number): Promise<void> {
		// currentTarget and the pre-action order are only valid during dispatch.
		const button = event.currentTarget;
		const heldFocus = button instanceof HTMLElement && document.activeElement === button;
		const removedId = queue.tracks[index]?.id;
		const beforeIds = visibleEntries.map((entry) => entry.track.id);
		onremove?.(index);
		await tick();
		if (!focusStranded(heldFocus, button)) return;
		const current = new Set<string>();
		for (const row of list?.querySelectorAll('[data-testid="queue-item"]') ?? []) {
			const id = row.getAttribute('data-track-id');
			if (id !== null) current.add(id);
		}
		const target = survivorId(beforeIds, removedId, current);
		if (target !== null && focusRemoveFor(target)) return;
		// No surviving displayed row. When tracks survive but the filter
		// hides every one of them, the honest no-matches status takes
		// recovery focus; only a truly empty queue uses the empty target.
		if (queue.tracks.length > 0 && focusFilterEmptyTarget()) return;
		focusEmptyTarget();
	}

	/**
	 * Report the clear intent, then land focus on the stable empty target
	 * instead of the now-disabled Clear button or the body.
	 */
	async function reportClear(event: Event): Promise<void> {
		const button = event.currentTarget;
		const heldFocus = button instanceof HTMLElement && document.activeElement === button;
		onclear?.();
		await tick();
		if (!focusStranded(heldFocus, button)) return;
		focusEmptyTarget();
	}

	/**
	 * Pending destructive confirm. Panel-local on purpose: it dies with the
	 * panel (route unmount, access-refusal alert branch) and dismisses itself
	 * the moment its precondition goes away (queue emptied by an auth
	 * transition, nothing left upcoming), so it can never act on, or survive
	 * into, another account's queue.
	 */
	let pending: 'clear' | 'trim' | null = $state(null);
	let pendingTrigger: HTMLButtonElement | null = null;
	let cancelButtonEl: HTMLButtonElement | null = $state(null);
	let cancelNote = $state('');
	let cancelNoteVersion = $state(0);

	let pendingValid = $derived(
		pending === 'clear' ? trackCount > 0 : pending === 'trim' ? upcoming > 0 : false
	);

	$effect(() => {
		if (pending !== null && !pendingValid) pending = null;
	});

	async function openConfirm(kind: 'clear' | 'trim', event: Event): Promise<void> {
		const button = event.currentTarget;
		pendingTrigger = button instanceof HTMLButtonElement ? button : null;
		cancelNote = '';
		pending = kind;
		await tick();
		// Safe default first; Tab reaches Confirm next.
		cancelButtonEl?.focus({ preventScroll: true });
	}

	function requestClear(event: Event): void {
		if (!confirmDestructive) {
			void reportClear(event);
			return;
		}
		if (trackCount === 0) return;
		void openConfirm('clear', event);
	}

	function requestTrim(event: Event): void {
		if (!confirmDestructive) {
			void reportTrim(event, onclearupcoming);
			return;
		}
		if (upcoming <= 0) return;
		void openConfirm('trim', event);
	}

	/** Cancel is a pure UI reset: nothing is reported, focus returns to the trigger. */
	async function cancelConfirm(): Promise<void> {
		const trigger = pendingTrigger;
		const kind = pending;
		pending = null;
		pendingTrigger = null;
		cancelNote = kind === 'trim' ? 'Clear upcoming cancelled. Queue kept.' : 'Clear queue cancelled. Queue kept.';
		cancelNoteVersion += 1;
		await tick();
		trigger?.focus({ preventScroll: true });
	}

	async function acceptConfirm(): Promise<void> {
		const kind = pending;
		if (kind === null || !pendingValid) return;
		const trigger = pendingTrigger;
		pending = null;
		pendingTrigger = null;
		cancelNote = '';
		if (kind === 'clear') onclear?.();
		else onclearupcoming?.();
		await tick();
		// The Confirm button unmounted with focus on it (or never held it):
		// recover onto the same stable targets as the one-step flows, unless
		// focus has already moved on to a live element. A trigger that kept
		// focus while the action disabled it (an assistive-technology virtual
		// click activates Confirm without moving DOM focus) counts as
		// stranded under the same rule as the one-step focusStranded check —
		// the browser drops it to the body on the next rendering steps.
		const active = document.activeElement;
		const strandedOnTrigger =
			trigger instanceof HTMLButtonElement && trigger.disabled && active === trigger;
		if (!strandedOnTrigger && active !== null && active !== document.body) return;
		if (kind === 'clear') focusEmptyTarget();
		else {
			// A filtered-out current row stays hidden until explicitly revealed.
			const target = currentRowElement() ?? revealCurrentButtonEl;
			target?.focus({ preventScroll: true });
			target?.scrollIntoView({ block: 'nearest', behavior: 'instant' });
		}
	}

	let normalizedQuery = $derived(filterQuery.trim().toLowerCase());

	/** Blank titles render (and match) as their stable track id, never empty. */
	function displayTitle(track: Track): string {
		return track.title?.trim() ? track.title : track.id;
	}

	/**
	 * Shared presentation matcher: `normalized` is already trimmed and
	 * lowercased, and the shared export re-normalizes idempotently, so
	 * this delegation renders byte-identical output to the inline version.
	 */
	function matchesQuery(track: Track, normalized: string): boolean {
		return matchesQueueQuery(track, normalized);
	}

	/**
	 * Visible rows with their ORIGINAL queue indices: every row action,
	 * disabled boundary and the current marker use the full-queue slot, never
	 * the filtered position. Pure reads only — frozen props stay untouched.
	 */
	let visibleEntries = $derived(
		queue.tracks
			.map((track, index) => ({ track, index }))
			.filter(({ track }) => matchesQuery(track, normalizedQuery))
	);
	/**
	 * Explicit reveal only: bound to the Show-current-track button's click.
	 * Clears a hiding filter first, then focuses/reveals the real row.
	 * Never runs from an effect, never mutates state, never calls transport.
	 * Focus uses preventScroll and the scroll is instant (`block: nearest`,
	 * never smooth), so no reduced-motion special case is needed.
	 */
	async function revealCurrent(): Promise<void> {
		if (selectedIndex < 0) return;
		if (!visibleEntries.some((entry) => entry.index === selectedIndex)) {
			filterQuery = '';
			await tick();
		}
		const row = currentRowElement();
		if (!row) return;
		row.focus({ preventScroll: true });
		row.scrollIntoView({ block: 'nearest', behavior: 'instant' });
	}

	/**
	 * Report one trim intent, then land focus deterministically: a trim that
	 * removed the rows keeping its control enabled leaves a disabled button
	 * behind, so focus moves to the surviving current row instead of the
	 * body. Runs only on explicit activation — prop-only updates never reach
	 * here, so background refreshes cannot steal focus.
	 */
	async function reportTrim(event: Event, notify: (() => void) | undefined): Promise<void> {
		// currentTarget is only valid during dispatch; capture it before the tick.
		const button = event.currentTarget;
		notify?.();
		await tick();
		if (button instanceof HTMLButtonElement && button.disabled) {
			currentRowElement()?.focus({ preventScroll: true });
		}
	}

	/**
	 * Report one direct-selection intent with the ORIGINAL queue coordinate.
	 * Runs only on explicit activation — prop-only updates never reach here,
	 * so background refreshes cannot steal focus. A detached button (removed
	 * row) or an identity mismatch (stale closure over a reused slot) reports
	 * nothing, so a removed identity can never select its replacement. The
	 * host validates again and applies the selectTrack transition.
	 */
	function reportSelect(event: Event, index: number, trackId: string): void {
		const button = event.currentTarget;
		if (!(button instanceof HTMLButtonElement)) return;
		if (!button.isConnected) return;
		if (!onselect) return;
		if (queue.tracks[index]?.id !== trackId) return;
		if (!Number.isInteger(index) || index < 0 || index >= queue.tracks.length) return;
		onselect(index);
	}
</script>

<!--
	Reusable queue panel: renders a `QueueState` from the queue store with
	the same hooks the demo player and the queue-order specs rely on.
	The host page owns selection (via `onselect`), transport (via `onnext`)
	and management (via `onmove`/`onplaynext`/`onremove`/`onclear`); the
	panel itself never mutates state.
-->
<p data-testid="now-playing" role={liveRegions ? 'status' : undefined}>
	{#if current}
		<!-- String expression: Svelte trims the leading space of ` — ` after
			{#if}, rendering an asymmetric `Title— Artist`. -->
		Now playing: {displayTitle(current)}{#if current.artist}{' — '}{current.artist}{/if}
	{:else}
		Queue is empty
	{/if}
</p>

<p data-testid="queue-position">
	{#if trackCount === 0}
		Queue selection: Queue is empty
	{:else if selectedIndex < 0}
		Queue selection: No track selected, {trackCount}
		{trackCount === 1 ? 'track' : 'tracks'}
	{:else}
		Queue selection: Track {selectedIndex + 1} of {trackCount}, {upcoming} upcoming
	{/if}
</p>

<!-- Keep the live region mounted; refresh only its content for equal-title actions. -->
<p data-testid="queue-announcement" role={liveRegions ? 'status' : undefined}>{#key announcementVersion}{announcement}{/key}</p>

<button
	type="button"
	data-testid="player-next"
	disabled={!current}
	onclick={() => onnext?.()}
>
	Next
</button>

<button
	type="button"
	data-testid="queue-reveal-current"
	bind:this={revealCurrentButtonEl}
	disabled={selectedIndex < 0}
	onclick={revealCurrent}
>
	Show current track
</button>

<ShuffleUpcoming state={queue} {onshuffle} {radioActive} {liveRegions} />

<!--
	Queue-only filter: transient presentation state. It hides rows but
	never reorders, reselects, persists or fetches anything; the
	orientation summary above always describes the full queue.
-->
<div class="queue-filter">
	<label for="queue-filter-input">Filter queue</label>
	<input id="queue-filter-input" type="search" bind:value={filterQuery} />
	<button type="button" onclick={() => (filterQuery = '')}>Clear queue filter</button>
</div>

<p data-testid="queue-filter-count" role={liveRegions ? 'status' : undefined}>
	{visibleEntries.length} of {trackCount} tracks
</p>

{#if trackCount > 0 && visibleEntries.length === 0}
	<!--
		Zero-visible recovery target: programmatically focusable so a removal
		that leaves only hidden tracks behind lands here instead of the body.
		Focused only by explicit remove recovery, never on render; never a
		tab stop.
	-->
	<p data-testid="queue-filter-empty" tabindex="-1" bind:this={filterEmptyTarget}>No matches in this queue</p>
{/if}

<ol aria-label="Playback queue" bind:this={list}>
	<!-- Stable track identity preserves keyboard focus through reorders. The
		store guarantees uniqueness: addTrack dedupes and restoreQueue rejects duplicates. -->
	{#each visibleEntries as { track, index } (track.id)}
		<!-- svelte-ignore a11y_no_noninteractive_tabindex: tabindex is provably -1-or-absent (QO-U14); the current row is a programmatic focus target, never a tab stop. -->
		<li
			class="queue-row"
			data-testid="queue-item"
			data-track-id={track.id}
			aria-current={index === selectedIndex ? 'true' : undefined}
			tabindex={index === selectedIndex ? -1 : undefined}
		>
			{displayTitle(track)}{#if track.artist}{' — '}{track.artist}{/if}
			{#if ontogglefavorite}
				<button
					type="button"
					class="queue-star"
					aria-label="{favoriteIds.has(track.id) ? 'Unstar' : 'Star'} {displayTitle(track)}"
					aria-pressed={favoriteIds.has(track.id)}
					onclick={() => ontogglefavorite?.(track.id)}
				>
					<span aria-hidden="true">{favoriteIds.has(track.id) ? '★' : '☆'}</span>
				</button>
			{/if}
			{#if oncopylink}
				<button
					type="button"
					class="queue-copy-link"
					data-testid="queue-copy-link"
					aria-label="Copy link {displayTitle(track)}"
					onclick={() => oncopylink?.(track.id)}
				>
					Copy link
				</button>
			{/if}
			<button
				type="button"
				aria-label="Move {displayTitle(track)} up"
				disabled={index === 0}
				onclick={() => onmove?.(index, index - 1)}
			>
				Up
			</button>
			<button
				type="button"
				aria-label="Move {displayTitle(track)} down"
				disabled={index === trackCount - 1}
				onclick={() => onmove?.(index, index + 1)}
			>
				Down
			</button>
			{#if trackCount > 1}
				<button
					type="button"
					aria-label="Move {displayTitle(track)} to top"
					disabled={index === 0}
					onclick={() => onmove?.(index, 0)}
				>
					Move to top
				</button>
				<button
					type="button"
					aria-label="Move {displayTitle(track)} to bottom"
					disabled={index === trackCount - 1}
					onclick={() => onmove?.(index, trackCount - 1)}
				>
					Move to bottom
				</button>
			{/if}
			<button
				type="button"
				aria-label="Play {displayTitle(track)} next"
				onclick={() => onplaynext?.(index)}
			>
				Next
			</button>
			<button
				type="button"
				aria-label="Remove {displayTitle(track)} from queue"
				onclick={(event) => void reportRemove(event, index)}
			>
				Remove
			</button>
			{#if onselect}
				<button
					type="button"
					aria-label="Select {displayTitle(track)}"
					onclick={(event) => reportSelect(event, index, track.id)}
				>
					Select
				</button>
			{/if}
		</li>
	{/each}
</ol>

{#if onkeepcurrentandupcoming}
	<button
		type="button"
		disabled={selectedIndex <= 0}
		onclick={(event) => void reportTrim(event, onkeepcurrentandupcoming)}
	>
		Keep current and upcoming
	</button>
{/if}
{#if onclearupcoming}
	<button
		type="button"
		disabled={selectedIndex < 0 || selectedIndex >= trackCount - 1}
		onclick={requestTrim}
	>
		Clear upcoming
	</button>
{/if}

{#if trackCount === 0}
	<!--
		Stable recovery target when no rows survive: a programmatically
		focusable status line, never a control, tab stop or modal. Focused
		only by explicit remove/clear recovery, never on render.
	-->
	<p data-testid="queue-empty-focus" tabindex="-1" bind:this={emptyTarget}>Queue is empty</p>
{/if}

<button
	type="button"
	disabled={queue.tracks.length === 0}
	onclick={requestClear}
>
	Clear queue
</button>

{#if pending !== null && pendingValid}
	<!-- A fieldset keeps the prompt a labelled group without a dialog trap:
		the rest of the page stays reachable and nothing is modal. -->
	<fieldset class="queue-confirm" data-testid="queue-clear-confirm">
		<legend>{pending === 'clear' ? 'Clear queue?' : 'Clear upcoming?'}</legend>
		<p id="{confirmId}-message">
			{#if pending === 'clear'}
				Remove all {trackCount} {trackCount === 1 ? 'track' : 'tracks'} from the queue? This cannot be undone.
			{:else}
				Remove {upcoming} upcoming {upcoming === 1 ? 'track' : 'tracks'} from the queue? The current track stays. This cannot be undone.
			{/if}
		</p>
		<div class="queue-confirm-actions">
			<button
				type="button"
				bind:this={cancelButtonEl}
				aria-describedby="{confirmId}-message"
				onclick={() => void cancelConfirm()}
			>
				Cancel
			</button>
			<button type="button" onclick={() => void acceptConfirm()}>
				{pending === 'clear' ? 'Yes, clear queue' : 'Yes, clear upcoming'}
			</button>
		</div>
	</fieldset>
{/if}

<!-- Polite outcome for a cancelled confirm; the queue-announcement region
	stays reserved for real queue changes. -->
<p data-testid="queue-confirm-feedback" aria-live={liveRegions ? 'polite' : undefined}>{#key cancelNoteVersion}{cancelNote}{/key}</p>

<style>
	.queue-filter {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: var(--nt-space-2);
	}

	.queue-filter input {
		flex: 1 1 12rem;
		min-width: 0;
		max-width: 100%;
		min-height: 2.75rem;
		padding: var(--nt-space-2) var(--nt-space-3);
		font: inherit;
		color: var(--nt-fg);
		background: var(--nt-bg-raised);
		border: 1px solid var(--nt-border);
		border-radius: var(--nt-radius);
	}

	/* Visible keyboard focus ring even where the app stylesheet is absent
	 * (mounted test parent); the app's own :focus-visible agrees with this. */
	.queue-filter input:focus-visible {
		outline: 2px solid var(--nt-accent);
		outline-offset: 2px;
	}

	/* Touch-sized clear action matching the input row. */
	.queue-filter button {
		min-height: 2.75rem;
		padding: var(--nt-space-2) var(--nt-space-3);
		font: inherit;
	}

	.queue-star {
		min-width: 2.75rem;
		min-height: 2.75rem;
		font: inherit;
		color: inherit;
		cursor: pointer;
		vertical-align: middle;
	}

	.queue-copy-link {
		min-height: 2.75rem;
		font: inherit;
		cursor: pointer;
	}

	.queue-star:focus-visible,
	.queue-copy-link:focus-visible {
		outline: 2px solid var(--nt-accent, currentColor);
		outline-offset: 2px;
	}

	.queue-confirm {
		min-width: 0;
		margin-inline: 0;
		padding: var(--nt-space-2) var(--nt-space-3);
		color: var(--nt-fg);
		background: var(--nt-bg-raised);
		border: 1px solid var(--nt-border);
		border-radius: var(--nt-radius);
	}

	.queue-confirm p {
		overflow-wrap: anywhere;
	}

	.queue-confirm-actions {
		display: flex;
		flex-wrap: wrap;
		gap: var(--nt-space-2) var(--nt-space-3);
	}

	/* Touch-sized, with a ring that holds without the app stylesheet. */
	.queue-confirm button {
		min-height: 2.75rem;
		padding: var(--nt-space-2) var(--nt-space-3);
		font: inherit;
	}

	.queue-confirm button:focus-visible {
		outline: 2px solid var(--nt-accent, currentColor);
		outline-offset: 2px;
	}

	.queue-row {
		overflow-wrap: anywhere;
		/* Clear the 2px outline + 2px offset when nearest-scroll rounds
		 * fractional line heights at the viewport edge. */
		scroll-margin-block: 4px;
	}

	/* Programmatic focus target for explicit reveal: the ring must hold
	 * even where the app stylesheet is absent (mounted test parent). */
	.queue-row:focus {
		outline: 2px solid var(--nt-accent, currentColor);
		outline-offset: 2px;
	}

	/* Same ring for the empty-queue and zero-visible recovery targets:
	 * keyboard recovery must stay visible without relying on the host
	 * page's stylesheet. */
	[data-testid='queue-empty-focus'],
	[data-testid='queue-filter-empty'] {
		scroll-margin-block: 4px;
	}

	[data-testid='queue-empty-focus']:focus,
	[data-testid='queue-filter-empty']:focus {
		outline: 2px solid var(--nt-accent, currentColor);
		outline-offset: 2px;
	}
</style>
