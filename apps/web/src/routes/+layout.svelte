<script lang="ts">
	import { tick, untrack } from 'svelte';
	import { afterNavigate } from '$app/navigation';
	import favicon from '#lib/assets/favicon.svg';
	import PaywallModal from '#lib/billing/PaywallModal.svelte';
	import { onPaywall, type PaywallModel } from '#lib/billing/planLimit.js';
	import '../app.css';
	import type { LayoutProps } from './$types';

	let { children }: LayoutProps = $props();

	type Appearance = 'system' | 'light' | 'dark';

	// Device-local appearance choice. Only the raw validated enum is ever
	// stored — no identity, no backend contact. Missing, malformed or
	// unreadable storage means System, which follows the OS.
	const APPEARANCE_KEY = 'nntune:appearance:v1';
	const APPEARANCE_ATTRIBUTE = 'data-appearance';

	let appearance = $state<Appearance>('system');

	function readStoredAppearance(): Appearance {
		try {
			const raw = window.localStorage.getItem(APPEARANCE_KEY);
			return raw === 'light' || raw === 'dark' || raw === 'system' ? raw : 'system';
		} catch {
			return 'system';
		}
	}

	function applyAppearance(value: Appearance): void {
		if (value === 'system') {
			document.documentElement.removeAttribute(APPEARANCE_ATTRIBUTE);
		} else {
			document.documentElement.setAttribute(APPEARANCE_ATTRIBUTE, value);
		}
	}

	function onAppearanceChange(event: Event): void {
		const value = (event.currentTarget as HTMLSelectElement).value as Appearance;
		appearance = value;
		applyAppearance(value);
		try {
			window.localStorage.setItem(APPEARANCE_KEY, value);
		} catch {
			// Storage refused: the in-memory choice still applies for this
			// session; the next load falls back to System.
		}
	}

	$effect(() => {
		// Client-only ($effect never runs during SSR): restore the saved
		// choice over the server-rendered System default, then track OS
		// changes while System is selected. Exactly one media listener for
		// the layout's lifetime — choices and client navigations never add
		// more, and unmount removes it.
		const initial = readStoredAppearance();
		appearance = initial;
		applyAppearance(initial);
		const media = window.matchMedia('(prefers-color-scheme: dark)');
		const onChange = () => {
			if (untrack(() => appearance) === 'system') applyAppearance('system');
		};
		media.addEventListener('change', onChange);
		return () => media.removeEventListener('change', onChange);
	});

	// Any 402/429 limit response from an API client lands here as a paywall
	// modal (upgrade CTA, not a dead-end toast). See planLimit.ts.
	let paywall = $state<PaywallModel | null>(null);

	let lastFocused: HTMLElement | null = null;
	let opener: HTMLElement | null = null;
	let restoreAfterNavigation = false;

	$effect(() => {
		// Remember focus before an in-flight action disables its own control.
		const rememberFocus = (event: FocusEvent) => {
			if (event.target instanceof HTMLElement && !event.target.closest('dialog')) {
				lastFocused = event.target;
			}
		};
		document.addEventListener('focusin', rememberFocus);
		const unsubscribe = onPaywall((model) => {
			if (!untrack(() => paywall)) {
				const active = document.activeElement;
				opener = active instanceof HTMLElement && active !== document.body
					? active : lastFocused;
			}
			paywall = model;
		});
		return () => {
			document.removeEventListener('focusin', rememberFocus);
			unsubscribe();
		};
	});

	function restoreFocus() {
		if (opener?.isConnected && !opener.matches(':disabled, [inert]') && opener.getClientRects().length) {
			opener.focus();
			if (document.activeElement === opener) return;
		}
		const fallback = document.querySelector<HTMLElement>('main')
			?? document.querySelector<HTMLElement>('a[href], button:not(:disabled), input:not(:disabled)');
		if (fallback) {
			if (!fallback.hasAttribute('tabindex')) fallback.setAttribute('tabindex', '-1');
			fallback.focus();
		}
	}

	async function dismissPaywall(retry?: () => void, navigating = false) {
		paywall = null;
		restoreAfterNavigation = navigating;
		// Let Retry update the opener's disabled state before choosing a focus
		// target, rather than focusing a control that immediately disables.
		retry?.();
		await tick();
		if (paywall !== null) return; // A new limit response now owns focus.
		restoreFocus();
	}

	// SvelteKit resets focus after link navigation; use the destination main
	// when Upgrade unmounted the original control, not a detached opener.
	afterNavigate(() => {
		if (!restoreAfterNavigation) return;
		restoreAfterNavigation = false;
		restoreFocus();
	});
</script>

<svelte:head>
	<link rel="icon" href={favicon} />
	<link rel="manifest" href="/manifest.webmanifest" />
	<meta name="theme-color" content="#0b0d12" />
</svelte:head>

<div class="nt-appearance nt-wrap">
	<label for="nt-appearance-select">Appearance</label>
	<select id="nt-appearance-select" value={appearance} onchange={onAppearanceChange}>
		<option value="system">System</option>
		<option value="light">Light</option>
		<option value="dark">Dark</option>
	</select>
</div>

{@render children()}

{#if paywall}
	<PaywallModal model={paywall} onclose={dismissPaywall} />
{/if}
