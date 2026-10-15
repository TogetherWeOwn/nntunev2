<script lang="ts">
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
	// Local ticking copy of the server-provided wait; the model stays frozen.
	// The effect below resynchronizes it when another limit response updates
	// this still-open modal.
	// svelte-ignore state_referenced_locally
	let remaining = $state<number | null>(model.retryAfterSeconds);

	$effect(() => {
		remaining = model.retryAfterSeconds;
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

	// Tick the retry countdown down to zero; at zero the text flips to
	// "You can try again now." and the interval stops.
	$effect(() => {
		if (remaining === null || remaining <= 0) return;
		const timer = setInterval(() => {
			remaining = remaining === null ? null : Math.max(0, remaining - 1);
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
		<a class="nt-button" href={model.ctaHref}>Upgrade your plan</a>
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
