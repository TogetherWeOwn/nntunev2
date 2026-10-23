<script lang="ts">
	import { tick, untrack } from 'svelte';
	import { afterNavigate } from '$app/navigation';
	import { page } from '$app/state';
	import favicon from '#lib/assets/favicon.svg';
	import PaywallModal from '#lib/billing/PaywallModal.svelte';
	import { onPaywall, type PaywallModel } from '#lib/billing/planLimit.js';
	import '../app.css';
	import type { LayoutProps } from './$types';

	let { children }: LayoutProps = $props();

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

	// App-shell navigation (roadmap NAV): Library, local NZB selection and
	// Account, reachable from every page without typing URLs. Static native
	// links only: the shell never fetches, uploads or processes anything.
	type ShellDestination = 'library' | 'import' | 'account';

	function matchesBoundary(pathname: string, base: string): boolean {
		return pathname === base || pathname.startsWith(`${base}/`);
	}

	// Longest base first so nested destinations win: exactly the
	// most-specific boundary match carries aria-current.
	const currentDestination = $derived.by((): ShellDestination | null => {
		const pathname = page.url.pathname;
		if (matchesBoundary(pathname, '/library/import')) return 'import';
		if (matchesBoundary(pathname, '/library')) return 'library';
		if (matchesBoundary(pathname, '/account')) return 'account';
		return null;
	});

	// Skip link: move keyboard focus to the page's own <main> (each route
	// renders exactly one) without adding nested landmarks.
	function skipToContent(event: MouseEvent): void {
		const main = document.querySelector('main');
		if (!main) return;
		event.preventDefault();
		if (!main.hasAttribute('tabindex')) main.setAttribute('tabindex', '-1');
		main.focus();
	}
</script>

<svelte:head>
	<link rel="icon" href={favicon} />
	<link rel="manifest" href="/manifest.webmanifest" />
	<meta name="theme-color" content="#0b0d12" />
</svelte:head>

<a class="skip-link" href="#main-content" onclick={skipToContent}>Skip to content</a>
<nav aria-label="Primary" class="nt-wrap shell-nav">
	<a href="/library" aria-current={currentDestination === 'library' ? 'page' : undefined}>
		Library
	</a>
	<a href="/library/import" aria-current={currentDestination === 'import' ? 'page' : undefined}>
		Select NZB locally
	</a>
	<a href="/account/billing" aria-current={currentDestination === 'account' ? 'page' : undefined}>
		Account
	</a>
</nav>

{@render children()}

{#if paywall}
	<PaywallModal model={paywall} onclose={dismissPaywall} />
{/if}

<style>
	.skip-link {
		position: absolute;
		inset-inline-start: var(--nt-space-3);
		top: var(--nt-space-2);
		z-index: 10;
		padding: var(--nt-space-2) var(--nt-space-3);
		border-radius: var(--nt-radius);
		background: var(--nt-bg-raised);
		color: var(--nt-fg);
		transform: translateY(-300%);
	}

	.skip-link:focus-visible {
		transform: none;
	}

	.shell-nav {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: var(--nt-space-2) var(--nt-space-4);
		padding-block: var(--nt-space-2);
		border-bottom: 1px solid var(--nt-border);
	}

	.shell-nav a {
		padding: var(--nt-space-1) var(--nt-space-2);
		border-radius: var(--nt-radius);
		color: var(--nt-fg-muted);
		text-decoration: none;
	}

	.shell-nav a[aria-current='page'] {
		color: var(--nt-fg);
		font-weight: 600;
		text-decoration: underline;
	}
</style>
