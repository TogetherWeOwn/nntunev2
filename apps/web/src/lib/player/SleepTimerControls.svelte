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
	let minutesEl: HTMLInputElement;
	let tracksEl: HTMLInputElement;
	let minutesError = $state('');
	let tracksError = $state('');

	function clearErrors(): void {
		minutesError = tracksError = '';
	}
	function arm(mode: SleepTimerMode, message: string): void {
		clearErrors();
		start(mode, message);
	}
	function armDuration(): void {
		const raw = minutesEl.value.trim();
		const ms = Number(raw) * 60_000;
		if (!Number.isFinite(ms) || ms <= 0) {
			minutesError = 'Enter a positive finite number of minutes.';
			return;
		}
		arm({ kind: 'duration', ms }, `Sleep in ${raw} minute${Number(raw) === 1 ? '' : 's'}`);
	}
	function armStopAfter(): void {
		const count = Number(tracksEl.value);
		if (!Number.isInteger(count) || count < 1) {
			tracksError = 'Enter a whole positive integer count of tracks.';
			return;
		}
		arm({ kind: 'stop-after-tracks', count }, `Sleep after ${count} track${count === 1 ? '' : 's'}`);
	}
</script>

<div class="sleep-row">
	<label for={`${id}-minutes`}>Sleep timer (minutes)</label>
	<input id={`${id}-minutes`} type="number" min="1" value="15" data-testid="sleep-timer-minutes"
		bind:this={minutesEl} aria-invalid={minutesError ? 'true' : undefined}
		aria-describedby={minutesError ? `${id}-minutes-error` : undefined}
		aria-errormessage={minutesError ? `${id}-minutes-error` : undefined} />
	<button class="nt-button nt-button--ghost" type="button" data-testid="sleep-timer-set-duration" onclick={armDuration}>
		Sleep after duration
	</button>
	{#if minutesError}<p id={`${id}-minutes-error`} class="sleep-error" role="alert">{minutesError}</p>{/if}
</div>
<button class="nt-button nt-button--ghost" type="button" data-testid="sleep-timer-set-end-of-track"
	onclick={() => arm({ kind: 'end-of-track' }, 'Sleep at end of track')}>
	Sleep at end of track
</button>
<div class="sleep-row">
	<label for={`${id}-tracks`}>Stop after tracks</label>
	<input id={`${id}-tracks`} type="number" min="1" step="1" value="2" data-testid="sleep-timer-tracks"
		bind:this={tracksEl} aria-invalid={tracksError ? 'true' : undefined}
		aria-describedby={tracksError ? `${id}-tracks-error` : undefined}
		aria-errormessage={tracksError ? `${id}-tracks-error` : undefined} />
	<button class="nt-button nt-button--ghost" type="button" data-testid="sleep-timer-set-stop-after" onclick={armStopAfter}>
		Sleep after N tracks
	</button>
	{#if tracksError}<p id={`${id}-tracks-error`} class="sleep-error" role="alert">{tracksError}</p>{/if}
</div>
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
