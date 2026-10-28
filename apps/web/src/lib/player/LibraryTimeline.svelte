<script lang="ts">
	import { untrack } from 'svelte';
	import { deriveTrackTime } from '#lib/format.js';
	import { readTimeline, seekTarget, type TimelineSnapshot } from '#lib/player/libraryTimeline.js';
	import { SEEK_SECONDS } from '#lib/player/shortcuts.js';

	interface Props {
		/** The live native audio this timeline belongs to (null until mounted). */
		element: HTMLAudioElement | undefined;
		/** Reads the page's still-current audio element at activation time. */
		getCurrent: () => HTMLAudioElement | undefined;
		/** The resolved stream source this audio was keyed on. */
		source: string;
	}

	let { element, getCurrent, source }: Props = $props();

	const EVENTS = [
		'loadedmetadata',
		'durationchange',
		'timeupdate',
		'seeking',
		'seeked',
		'emptied',
		'loadstart',
		// Buffering can add seekable ranges while paused, with no timeupdate.
		'progress',
		'canplay'
	] as const;

	let snapshot: TimelineSnapshot = $state({
		currentTime: 0,
		duration: null,
		ranges: [],
		seekable: false
	});
	let mode = $state<'elapsed' | 'remaining'>('elapsed');
	let failure = $state('');
	let slider: HTMLInputElement | undefined = $state();
	let shownKey = '';
	let frame = 0;

	const times = $derived(deriveTrackTime(snapshot.currentTime, snapshot.duration ?? Number.NaN));
	const effectiveMode: 'elapsed' | 'remaining' = $derived(
		snapshot.duration === null ? 'elapsed' : mode
	);
	const readout = $derived.by(() => {
		if (snapshot.duration === null) return `Elapsed ${times.elapsed} · Duration unavailable`;
		const lead =
			effectiveMode === 'remaining'
				? `Remaining ${times.remaining}`
				: `Elapsed ${times.elapsed}`;
		const tail = snapshot.seekable ? '' : ' · Seeking unavailable';
		return `${lead} of ${times.duration}${tail}`;
	});
	const sliderMax = $derived(snapshot.duration === null ? 0 : Math.ceil(snapshot.duration));

	// An element only counts while it is the page's current, attached audio
	// for the source this timeline was keyed on; anything else is stale.
	function live(el: HTMLAudioElement | undefined): el is HTMLAudioElement {
		return (
			el !== undefined &&
			el === element &&
			el === getCurrent() &&
			el.isConnected &&
			el.getAttribute('src') === source
		);
	}

	function refresh(el: HTMLAudioElement): void {
		const next = readTimeline(el);
		const key = `${Math.floor(next.currentTime)}|${next.duration}|${next.seekable}|${next.ranges.join(';')}`;
		if (key === shownKey) return;
		shownKey = key;
		snapshot = next;
	}

	// Coalesce bursts of media events into at most one visual update per frame.
	function schedule(el: HTMLAudioElement): void {
		if (frame !== 0) return;
		frame = requestAnimationFrame(() => {
			frame = 0;
			if (live(el)) refresh(el);
		});
	}

	$effect(() => {
		const el = element;
		if (!el) return;
		untrack(() => {
			shownKey = '';
			failure = '';
			refresh(el);
		});
		const onMedia = () => schedule(el);
		for (const type of EVENTS) el.addEventListener(type, onMedia);
		return () => {
			for (const type of EVENTS) el.removeEventListener(type, onMedia);
			if (frame !== 0) cancelAnimationFrame(frame);
			frame = 0;
		};
	});

	function seek(pick: (current: TimelineSnapshot) => number): void {
		const el = getCurrent();
		if (!live(el)) return;
		const target = seekTarget(readTimeline(el), pick(readTimeline(el)));
		if (target === null) {
			failure = 'Seeking is not available for this audio.';
			refresh(el);
			if (slider) slider.value = String(Math.floor(snapshot.currentTime));
			return;
		}
		try {
			el.currentTime = target;
		} catch {
			failure = "Couldn't seek this audio. Use the player controls instead.";
			if (slider) slider.value = String(Math.floor(snapshot.currentTime));
			return;
		}
		failure = '';
		refresh(el);
		// A clamped seek may leave the whole-second snapshot unchanged, while
		// the native range input has already moved to the requested value.
		if (slider) slider.value = String(Math.floor(snapshot.currentTime));
	}

	function onSlide(event: Event & { currentTarget: HTMLInputElement }): void {
		const requested = Number(event.currentTarget.value);
		seek(() => requested);
	}
</script>

<div class="nt-timeline" role="region" aria-label="Track timeline">
	<input
		bind:this={slider}
		class="nt-timeline-slider"
		data-testid="library-timeline"
		type="range"
		aria-label="Seek position in current track"
		aria-valuetext={readout}
		min="0"
		max={sliderMax}
		step="1"
		value={Math.floor(snapshot.currentTime)}
		disabled={!snapshot.seekable}
		oninput={onSlide}
	/>
	<div class="nt-timeline-row">
		<button
			type="button"
			data-testid="library-seek-back"
			disabled={!snapshot.seekable}
			onclick={() => seek((current) => current.currentTime - SEEK_SECONDS)}
		>
			Back {SEEK_SECONDS} seconds
		</button>
		<button
			type="button"
			data-testid="library-seek-forward"
			disabled={!snapshot.seekable}
			onclick={() => seek((current) => current.currentTime + SEEK_SECONDS)}
		>
			Forward {SEEK_SECONDS} seconds
		</button>
		<button
			type="button"
			data-testid="library-time-mode"
			disabled={snapshot.duration === null}
			onclick={() => (mode = effectiveMode === 'elapsed' ? 'remaining' : 'elapsed')}
		>
			{effectiveMode === 'elapsed' ? 'Show remaining time' : 'Show elapsed time'}
		</button>
	</div>
	<p class="nt-timeline-readout" data-testid="library-time-readout">{readout}</p>
	<p class="nt-timeline-status" data-testid="library-seek-status" role="status">{failure}</p>
</div>

<style>
	.nt-timeline {
		display: flex;
		flex-direction: column;
		gap: var(--nt-space-2);
		max-width: 100%;
		margin-block: var(--nt-space-2);
	}
	.nt-timeline-slider {
		box-sizing: border-box;
		width: 100%;
		min-height: 2.75rem;
		margin: 0;
	}
	.nt-timeline-row {
		display: flex;
		flex-wrap: wrap;
		gap: var(--nt-space-2);
	}
	.nt-timeline-row button {
		min-height: 2.75rem;
		min-width: 2.75rem;
		padding: var(--nt-space-2) var(--nt-space-3);
		font: inherit;
		color: var(--nt-fg);
		background: var(--nt-bg-raised);
		border: 1px solid var(--nt-border);
		border-radius: var(--nt-radius);
	}
	.nt-timeline-row button:disabled {
		opacity: 0.6;
	}
	.nt-timeline-slider:focus-visible,
	.nt-timeline-row button:focus-visible {
		outline: 2px solid currentColor;
		outline-offset: 3px;
	}
	.nt-timeline p {
		margin: 0;
		overflow-wrap: anywhere;
		color: var(--nt-fg);
	}
</style>
