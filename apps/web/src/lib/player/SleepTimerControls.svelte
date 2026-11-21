<script lang="ts">
	import type { SleepTimerMode } from './sleepTimer.js';

	let { armed, remaining, announcement, start, cancel }: {
		armed: boolean;
		remaining: string;
		announcement: string;
		start: (mode: SleepTimerMode, message: string) => void;
		cancel: () => void;
	} = $props();
	const id = $props.id();
	type Field = 'minutes' | 'tracks';
	const errors = $state({ minutes: '', tracks: '' });
	function clearErrors(): void { errors.minutes = errors.tracks = ''; }
	function arm(mode: SleepTimerMode, message: string): void {
		clearErrors();
		start(mode, message);
	}
	function submit(field: Field, raw: string): void {
		const count = Number(raw);
		const plural = count === 1 ? '' : 's';
		if (field === 'minutes') {
			const ms = count * 60_000;
			if (!Number.isFinite(ms) || ms <= 0) errors.minutes = 'Enter a positive finite number of minutes.';
			else arm({ kind: 'duration', ms }, `Sleep in ${raw} minute${plural}`);
		} else if (!Number.isInteger(count) || count < 1) errors.tracks = 'Enter a whole positive integer count of tracks.';
		else arm({ kind: 'stop-after-tracks', count }, `Sleep after ${count} track${plural}`);
	}
</script>

{#snippet entry(field: Field, label: string, value: number, action: string, button: string)}
	{@const errorId = errors[field] ? `${id}-${field}-error` : undefined}
	<form class="sleep-row" novalidate onsubmit={(event) => {
		event.preventDefault();
		submit(field, String(new FormData(event.currentTarget).get(field) ?? '').trim());
	}}>
		<label for={`${id}-${field}`}>{label}</label>
		<input id={`${id}-${field}`} name={field} type="number" min="1" {value} data-testid={`sleep-timer-${field}`}
			aria-invalid={errorId ? 'true' : undefined} aria-describedby={errorId} aria-errormessage={errorId} />
		<button class="nt-button nt-button--ghost" type="submit" data-testid={`sleep-timer-set-${action}`}>{button}</button>
		{#if errorId}<p id={errorId} class="sleep-error" role="alert">{errors[field]}</p>{/if}
	</form>
{/snippet}
{@render entry('minutes', 'Sleep timer (minutes)', 15, 'duration', 'Sleep after duration')}
<button class="nt-button nt-button--ghost" type="button" data-testid="sleep-timer-set-end-of-track"
	onclick={() => arm({ kind: 'end-of-track' }, 'Sleep at end of track')}>
	Sleep at end of track
</button>
{@render entry('tracks', 'Stop after tracks', 2, 'stop-after', 'Sleep after N tracks')}
<button class="nt-button nt-button--ghost" type="button" data-testid="sleep-timer-cancel"
	disabled={!armed} onclick={() => { clearErrors(); cancel(); }}>
	Cancel sleep timer
</button>
{#if armed && remaining}<p data-testid="sleep-timer-remaining">{remaining}</p>{/if}
<p data-testid="sleep-timer-status" aria-live="polite">{announcement}</p>
<p>Duration timers use wall-clock time and continue while paused.</p>

<style>
	.sleep-row {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: var(--nt-space-2);
	}
	button, input { min-height: 2.75rem; }
	input {
		flex: 0 1 8rem;
		min-width: 0;
		max-width: 100%;
		padding: var(--nt-space-2) var(--nt-space-3);
		font: inherit;
		color: var(--nt-fg);
		background: var(--nt-bg-raised);
		border: 1px solid var(--nt-border);
		border-radius: var(--nt-radius);
	}
	.sleep-error {
		flex-basis: 100%;
		margin: 0;
		color: var(--nt-bad);
		overflow-wrap: anywhere;
	}
</style>
