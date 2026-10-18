<script lang="ts">
	import { untrack } from 'svelte';
	import {
		formatCountdown,
		trackPaywallShown,
		type PaywallModel,
		type TrackFn
	} from './planLimit.js';

	interface Props {
		model: PaywallModel;
		onclose: () => void;
		/**
		 * Analytics sink; defaults to re-dispatching the documented event as a
		 * same-named CustomEvent on window (see planLimit.ts). Props only —
		 * never PII.
		 */
		track?: TrackFn;
	}

	let { model, onclose, track }: Props = $props();

	let dialog: HTMLDialogElement | undefined = $state();
	// Local ticking copy of the server-provided wait, counted from a
	// per-model deadline in wall-clock time; the model stays frozen. The
	// effect below resynchronizes both when another limit response updates
	// this still-open modal, so a fresh response starts a fresh deadline.
	// svelte-ignore state_referenced_locally
	let remaining = $state<number | null>(model.retryAfterSeconds);
	// svelte-ignore state_referenced_locally
	let deadline = $state<number | null>(
		model.retryAfterSeconds === null ? null : Date.now() + model.retryAfterSeconds * 1000
	);

	$effect(() => {
		// Track the model object itself (not just its seconds) so a new
		// limit response with the same wait still starts a fresh deadline.
		const current = model;
		const seconds = current.retryAfterSeconds;
		remaining = seconds;
		deadline = seconds === null ? null : Date.now() + seconds * 1000;
	});

	const countdownText = $derived(
		remaining === null ? model.countdownText : formatCountdown(remaining)
	);

	const defaultTrack: TrackFn = (name, props) => {
		window.dispatchEvent(new CustomEvent(name, { detail: props }));
	};

	let displayed = false;

	// Fire the analytics event exactly once per modal mount, and open natively
	// so Esc, focus trapping, and ::backdrop come from the platform. Effects
	// may re-run when props/bindings update; never re-open an open dialog.
	$effect(() => {
		if (displayed || !dialog) return;
		displayed = true;
		trackPaywallShown(track ?? defaultTrack, model);
		if (!dialog.open) dialog.showModal();
	});

	// Count down the retry wait by elapsed wall-clock time from the
	// per-model deadline: each callback recomputes what is left, so a
	// suspended tab (or throttled timer) that jumps forward lands on the
	// correct remaining time instead of one second less. At zero the text
	// flips to "You can try again now." and the interval stops; the
	// cleanup also disposes the timer when a fresh deadline replaces it,
	// when the wait is unknown/zero, or when the modal unmounts.
	$effect(() => {
		const currentDeadline = deadline;
		if (currentDeadline === null) return;
		const initial: number | null = untrack(() => remaining);
		if (initial === null || initial <= 0) return;
		const timer = setInterval(() => {
			const left = Math.max(0, Math.ceil((currentDeadline - Date.now()) / 1000));
			remaining = left;
			if (left <= 0) clearInterval(timer);
		}, 1000);
		return () => clearInterval(timer);
	});

	function close() {
		dialog?.close();
	}
</script>

<dialog
	bind:this={dialog}
	class="paywall"
	data-testid="plan-limit-notice"
	aria-labelledby="paywall-headline"
	aria-describedby="paywall-detail"
	onclose={onclose}
>
	<h2 id="paywall-headline">{model.headline}</h2>
	<p id="paywall-detail">{model.detail}</p>

	{#if model.kind === 'retryable' && countdownText}
		<!-- role="timer" (not aria-live): a per-second announcement would spam SR users. -->
		<p class="paywall-countdown" role="timer" data-testid="paywall-countdown">{countdownText}</p>
	{/if}

	<div class="paywall-actions">
		<a class="nt-button" href={model.ctaHref} onclick={close}>Upgrade your plan</a>
		<button type="button" class="nt-button nt-button--ghost" onclick={close}>Not now</button>
	</div>
</dialog>

<style>
	.paywall {
		border: 1px solid var(--nt-border);
		border-radius: var(--nt-radius);
		background: var(--nt-bg-raised);
		color: var(--nt-fg);
		max-width: min(28rem, calc(100vw - 2 * var(--nt-space-3)));
		padding: var(--nt-space-4);
	}

	.paywall::backdrop {
		background: rgb(0 0 0 / 0.55);
	}

	.paywall h2 {
		margin: 0 0 var(--nt-space-2);
		font-size: 1.25rem;
		line-height: 1.3;
	}

	.paywall p {
		margin: 0 0 var(--nt-space-3);
		color: var(--nt-fg-muted);
	}

	.paywall-countdown {
		font-weight: 600;
		color: var(--nt-fg);
		font-variant-numeric: tabular-nums;
	}

	.paywall-actions {
		display: flex;
		flex-wrap: wrap;
		gap: var(--nt-space-2);
	}
</style>
