<script lang="ts">
	import { onDestroy, tick, untrack } from 'svelte';
	import {
		formatCountdown,
		trackPaywallShown,
		type PaywallModel,
		type TrackFn
	} from './planLimit.js';

	interface Props {
		model: PaywallModel;
		onclose: (retry?: () => void, navigating?: boolean) => void;
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

	const shownModels = new WeakSet<PaywallModel>();
	const readyModels = new WeakSet<PaywallModel>();
	let hasShownModel = false;
	let announcement = $state('');
	let announcementVersion = 0;
	let closing = false;
	let retryAction: (() => void) | undefined;
	let navigating = false;

	async function announce(text: string) {
		const version = ++announcementVersion;
		// Empty first so equal-text replacement responses are still delivered.
		announcement = '';
		await tick();
		if (version === announcementVersion && !closing) announcement = text;
	}

	onDestroy(() => { announcementVersion += 1; });

	$effect(() => {
		// Identity, not seconds: equal waits from new responses get a fresh
		// deadline and one display event each, without timer-driven duplicates.
		const current = model;
		const seconds = current.retryAfterSeconds;
		remaining = seconds;
		deadline = seconds === null ? null : Date.now() + seconds * 1000;
		if (!shownModels.has(current)) {
			shownModels.add(current);
			untrack(() => trackPaywallShown(track ?? defaultTrack, current));
			if (hasShownModel && seconds !== 0) {
				void announce(`${current.headline}.${seconds === null ? '' : ` ${formatCountdown(seconds)}`}`);
			}
			hasShownModel = true;
		}
	});

	$effect(() => {
		if (model.kind === 'retryable' && remaining === 0 && !readyModels.has(model)) {
			readyModels.add(model);
			void announce(formatCountdown(0));
		}
	});

	const countdownText = $derived(
		remaining === null ? model.countdownText : formatCountdown(remaining)
	);

	const defaultTrack: TrackFn = (name, props) => {
		window.dispatchEvent(new CustomEvent(name, { detail: props }));
	};

	// Open natively for Esc and focus trapping; replacements never reopen it.
	$effect(() => {
		if (dialog && !dialog.open && !closing) dialog.showModal();
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
		if (closing) return;
		closing = true;
		announcementVersion += 1;
		dialog?.close();
	}

	function upgrade(event: MouseEvent) {
		if (!event.ctrlKey && !event.metaKey && !event.shiftKey && !event.altKey) {
			navigating = new URL(model.ctaHref, window.location.href).href !== window.location.href;
			// Already on pricing: don't run a redundant router navigation
			// that resets the focus we are returning to the plan control.
			if (!navigating) event.preventDefault();
		}
		close();
	}

	function retry() {
		retryAction = model.retry;
		close();
	}

	function backdrop(event: MouseEvent) {
		if (event.target !== dialog || !dialog) return;
		const bounds = dialog.getBoundingClientRect();
		if (event.clientX < bounds.left || event.clientX > bounds.right
			|| event.clientY < bounds.top || event.clientY > bounds.bottom) close();
	}

	// Native showModal trapping stutters in Chromium: Tab from the last
	// control lands on <body> once before wrapping back in (verified with
	// and without the app-shell nav in the DOM). Wrap explicitly at the
	// boundaries so keyboard focus never leaves the open dialog.
	function wrapTab(event: KeyboardEvent) {
		if (event.key !== 'Tab' || !dialog) return;
		const controls = [...dialog.querySelectorAll<HTMLElement>('a[href], button:not(:disabled)')]
			.filter((control) => control.getClientRects().length > 0);
		if (controls.length === 0) return;
		const first = controls[0];
		const last = controls[controls.length - 1];
		if (event.shiftKey && document.activeElement === first) {
			event.preventDefault();
			last.focus();
		} else if (!event.shiftKey && document.activeElement === last) {
			event.preventDefault();
			first.focus();
		}
	}
</script>

<dialog
	bind:this={dialog}
	class="paywall"
	data-testid="plan-limit-notice"
	aria-labelledby="paywall-headline"
	aria-describedby="paywall-detail"
	onclose={() => { closing = true; announcementVersion += 1; onclose(retryAction, navigating); }}
	oncancel={(event) => { event.preventDefault(); close(); }}
	onclick={backdrop}
	onkeydown={wrapTab}
>
	<h2 id="paywall-headline">{model.headline}</h2>
	<p id="paywall-detail">{model.detail}</p>

	{#if model.kind === 'retryable' && countdownText}
		<!-- role="timer" (not aria-live): a per-second announcement would spam SR users. -->
		<p class="paywall-countdown" role="timer" data-testid="paywall-countdown">{countdownText}</p>
	{/if}

	<p class="paywall-announcement" role="status" aria-live="polite" aria-atomic="true">{announcement}</p>

	<div class="paywall-actions">
		<a class="nt-button" href={model.ctaHref} onclick={upgrade}>Upgrade your plan</a>
		{#if model.kind === 'retryable'}
			<button type="button" class="nt-button nt-button--ghost" onclick={retry}
				disabled={remaining !== null && remaining > 0}>Retry</button>
		{/if}
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

	.paywall-announcement {
		position: absolute;
		width: 1px;
		height: 1px;
		padding: 0;
		overflow: hidden;
		clip-path: inset(50%);
		white-space: nowrap;
	}

	.paywall-actions button:disabled {
		opacity: 0.6;
		cursor: not-allowed;
	}

	.paywall-actions {
		display: flex;
		flex-wrap: wrap;
		gap: var(--nt-space-2);
	}
</style>
