<script lang="ts">
	import { tick, untrack } from 'svelte';
	import type { Snippet } from 'svelte';
	import { afterNavigate, goto } from '$app/navigation';
	import { page } from '$app/state';
	import { PUBLIC_API_BASE_URL } from '$app/env/public';
	import favicon from '#lib/assets/favicon.svg';
	import PaywallModal from '#lib/billing/PaywallModal.svelte';
	import { onPaywall, type PaywallModel } from '#lib/billing/planLimit.js';
	import { invalidateLibraryCache } from '#lib/library/cache.js';
	import { clearSignedInEmail } from '#lib/signed-in-email.js';
	import {
		notifyLocalAuthTransition,
		subscribeLocalAuthTransition
	} from '#lib/auth-transition.js';
	import { LIBRARY_AUTH_TRANSITION_KEY } from '#lib/library/cache.js';
	import {
		clearMiniPlayerHint,
		onMiniPlayerHint,
		type MiniPlayerHint
	} from '#lib/shell/mini-player.js';
	import {
		clearToasts,
		dismissToast,
		onToasts,
		pushToast,
		type Toast
	} from '#lib/shell/toasts.js';
	import { onRightPanel } from '#lib/shell/right-panel.js';
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
			// 'system' (or anything else unknown) falls through to the default.
			return raw === 'light' || raw === 'dark' ? raw : 'system';
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

	// Hydration marker for browser tests ($effect never runs during SSR, and
	// effects flush only after the whole tree has attached its handlers).
	// Without it a test can click a server-rendered form before hydration and
	// trigger a native GET submit instead of the client handler. Carries no state.
	$effect(() => {
		document.documentElement.setAttribute('data-hydrated', '');
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

	// App shell (roadmap W10): a top bar, a left rail and the main view on
	// desktop; a compact top bar, the main view, a mini-player and a tab bar
	// on narrow screens. Public routes (landing, auth, plans, demos) render
	// a slim public header instead — never the Primary landmark. Static
	// native links only: the shell never fetches, uploads or processes
	// anything by itself; the only request it can ever send is the sign-out
	// POST behind the account menu's Sign out control.
	function matchesBoundary(pathname: string, base: string): boolean {
		return pathname === base || pathname.startsWith(`${base}/`);
	}

	const PUBLIC_BASES = ['/login', '/signup', '/pricing', '/demo', '/auth'];

	const isPublicRoute = $derived.by((): boolean => {
		const pathname = page.url.pathname;
		return pathname === '/' || PUBLIC_BASES.some((base) => matchesBoundary(pathname, base));
	});

	type PrimaryDestination = 'home' | 'search' | 'library';

	// Longest base first so nested destinations win: exactly the
	// most-specific boundary match carries aria-current. Entity routes,
	// boundary look-alikes (/library2, /accountant) and everything outside
	// the three destinations mark nothing.
	const primaryDestination = $derived.by((): PrimaryDestination | null => {
		const pathname = page.url.pathname;
		if (matchesBoundary(pathname, '/home')) return 'home';
		if (matchesBoundary(pathname, '/search')) return 'search';
		if (matchesBoundary(pathname, '/library')) return 'library';
		return null;
	});

	// Account and settings are not primary destinations: their marker lives
	// on the opened account menu's entry, never on the rail or tab bar.
	const accountEntryCurrent = $derived.by((): boolean =>
		matchesBoundary(page.url.pathname, '/account')
	);
	const settingsEntryCurrent = $derived.by((): boolean =>
		matchesBoundary(page.url.pathname, '/settings')
	);

	// Closed menus contribute no tab stops or accessible controls.
	let openMenu = $state<'account' | null>(null);
	let menuOpener = $state<HTMLElement | null>(null);

	function focusFirstMenuItem(menuId: string): void {
		document
			.getElementById(menuId)
			?.querySelector<HTMLElement>('[role="menuitem"]:not([disabled])')
			?.focus();
	}

	async function toggleMenu(kind: 'account', event: MouseEvent): Promise<void> {
		const trigger = event.currentTarget as HTMLElement;
		if (openMenu === kind) {
			openMenu = null;
			menuOpener = null;
		} else {
			openMenu = kind;
			menuOpener = trigger;
			// Move focus from the trigger into the menu so keyboard users
			// land on the first item instead of tabbing from the button.
			await tick();
			focusFirstMenuItem('nt-account-menu');
		}
	}

	function closeMenus(refocusOpener = false): void {
		openMenu = null;
		if (refocusOpener && menuOpener?.isConnected) menuOpener.focus();
		menuOpener = null;
	}

	function onMenuKeydown(event: KeyboardEvent): void {
		if (event.key === 'Escape') {
			closeMenus(true);
			return;
		}
		if (event.key !== 'ArrowDown' && event.key !== 'ArrowUp') return;
		// Arrow keys travel between items within the open menu; the menu
		// keeps the roles, so traversal must not depend on Tab order.
		const root = (event.currentTarget as HTMLElement).closest('[data-menu-root]');
		const items = root
			? [...root.querySelectorAll<HTMLElement>('[role="menuitem"]:not([disabled])')]
			: [];
		if (items.length === 0) return;
		event.preventDefault();
		const active = document.activeElement as HTMLElement | null;
		const at = active ? items.indexOf(active) : -1;
		const next =
			event.key === 'ArrowDown'
				? items[(at + 1) % items.length]
				: items[(at - 1 + items.length) % items.length];
		next?.focus();
	}

	function onMenuFocusOut(event: FocusEvent): void {
		const root = event.currentTarget as HTMLElement;
		const next = event.relatedTarget as Node | null;
		// Focus left the menu root entirely (Tab out, pointer elsewhere):
		// drop the menu rather than leaving a detached panel behind.
		if (!next || !root.contains(next)) {
			openMenu = null;
			menuOpener = null;
		}
	}

	function onDocumentClick(event: MouseEvent): void {
		if (!openMenu) return;
		// Button toggles own their clicks; menu-item clicks navigate (which
		// closes via afterNavigate). Anything else dismisses the menu.
		if ((event.target as HTMLElement).closest('[data-menu-root]')) return;
		openMenu = null;
		menuOpener = null;
	}

	// SvelteKit resets focus after link navigation; use the destination main
	// when Upgrade unmounted the original control, not a detached opener.
	// Any navigation also drops an open menu.
	afterNavigate(() => {
		openMenu = null;
		menuOpener = null;
		if (!restoreAfterNavigation) return;
		restoreAfterNavigation = false;
		restoreFocus();
	});

	// Top-bar search: plain navigation to the Search page, never
	// search-as-you-type. No request fires until the Search page itself
	// loads its results from the library API.
	let searchText = $state('');

	function onSearchSubmit(event: SubmitEvent): void {
		event.preventDefault();
		const query = searchText.trim();
		searchText = '';
		goto(query ? `/search?q=${encodeURIComponent(query)}` : '/search');
	}

	// Shell display state, bridged from the shell stores into renderable
	// $state. Subscriptions live for the layout's lifetime.
	let toasts = $state<Toast[]>([]);
	let miniHint = $state<MiniPlayerHint | null>(null);
	let rightSnippet: Snippet | null = $state(null);

	// Shell display state is device-local, but Undo closures and the
	// mini-player hint capture the previous account's metadata. Any auth
	// boundary — this tab (security page, account menu) or another tab's
	// storage event — drops both so the next account starts clean.
	function clearShellAtAuthBoundary(): void {
		clearMiniPlayerHint();
		clearToasts();
	}

	$effect(() => {
		const offToasts = onToasts((list) => {
			toasts = [...list];
		});
		const offHint = onMiniPlayerHint((hint) => {
			miniHint = hint;
		});
		const offPanel = onRightPanel((panel) => {
			rightSnippet = panel;
		});
		const offAuth = subscribeLocalAuthTransition(clearShellAtAuthBoundary);
		const onStorage = (event: StorageEvent): void => {
			if (event.key === LIBRARY_AUTH_TRANSITION_KEY) clearShellAtAuthBoundary();
		};
		window.addEventListener('storage', onStorage);
		return () => {
			offToasts();
			offHint();
			offPanel();
			offAuth();
			window.removeEventListener('storage', onStorage);
		};
	});

	function onToastAction(toast: Toast): void {
		toast.action?.run();
		dismissToast(toast.id);
	}

	// Sign-out from the account menu. Same boundary crossing as the security
	// page (drop this tab's library snapshots and identity label, broadcast
	// the transition, then leave for /login); the API client stays out of
	// the layout chunk via a dynamic import. A refused sign-out keeps the
	// session and reports through a toast, never through chrome text.
	let signingOut = $state(false);

	async function signOut(): Promise<void> {
		if (signingOut) return;
		signingOut = true;
		closeMenus();
		try {
			const { ApiClient, ApiError } = await import('#lib/api/client.js');
			const client = new ApiClient({ baseUrl: PUBLIC_API_BASE_URL });
			try {
				await client.logout();
			} catch (error) {
				if (!(error instanceof ApiError) || error.status !== 401) {
					pushToast('Could not sign out. Please try again.');
					return;
				}
				// A 401 means the session is already gone: cross the boundary
				// exactly like a successful sign-out.
			}
		} catch {
			pushToast('Could not sign out. Please try again.');
			return;
		} finally {
			signingOut = false;
		}
		clearMiniPlayerHint();
		clearToasts();
		invalidateLibraryCache();
		clearSignedInEmail();
		notifyLocalAuthTransition();
		window.location.href = '/login';
	}

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

<svelte:document onclick={onDocumentClick} />

<a class="skip-link" href="#main-content" onclick={skipToContent}>Skip to content</a>

{#if isPublicRoute}
	<header class="nt-public nt-wrap" data-testid="shell-topbar">
		<a class="nt-brand" href="/">NNTune</a>
		<div class="nt-public-links">
			<a href="/pricing">Pricing</a>
			<a href="/login">Log in</a>
		</div>
		<!-- Same device-local control as the app top bar (one instance per
			page: the shells never co-render). Signed-out routes keep the
			theme switcher so the appearance journey stays intact. -->
		<div class="nt-appearance">
			<label for="nt-appearance-select">Appearance</label>
			<select id="nt-appearance-select" value={appearance} onchange={onAppearanceChange}>
				<option value="system">System</option>
				<option value="light">Light</option>
				<option value="dark">Dark</option>
			</select>
		</div>
	</header>
	{@render children()}
{:else}
	<div class="nt-topbar nt-wrap" data-testid="shell-topbar">
		<a class="nt-brand" href="/home">NNTune</a>
		<form class="nt-topbar-search" role="search" onsubmit={onSearchSubmit}>
			<input
				type="search"
				name="q"
				aria-label="Search"
				placeholder="Search"
				autocomplete="off"
				bind:value={searchText}
			/>
		</form>
		<div class="nt-appearance">
			<label for="nt-appearance-select">Appearance</label>
			<select id="nt-appearance-select" value={appearance} onchange={onAppearanceChange}>
				<option value="system">System</option>
				<option value="light">Light</option>
				<option value="dark">Dark</option>
			</select>
		</div>
		<div class="nt-menu-root" data-menu-root onfocusout={onMenuFocusOut}>
			<button
				type="button"
				aria-label="Account menu"
				aria-haspopup="menu"
				aria-expanded={openMenu === 'account'}
				aria-controls="nt-account-menu"
				onclick={(event) => toggleMenu('account', event)}
				onkeydown={onMenuKeydown}
			>
				Account
			</button>
			{#if openMenu === 'account'}
				<div class="nt-menu" role="menu" tabindex="-1" id="nt-account-menu" onkeydown={onMenuKeydown}>
					<a
						role="menuitem"
						href="/account/billing"
						aria-current={accountEntryCurrent ? 'page' : undefined}
					>
						Account
					</a>
					<a
						role="menuitem"
						href="/settings"
						aria-current={settingsEntryCurrent ? 'page' : undefined}
					>
						Settings
					</a>
					<button type="button" role="menuitem" onclick={signOut} disabled={signingOut}>
						Sign out
					</button>
				</div>
			{/if}
		</div>
	</div>
	<div class="nt-body">
		<div class="nt-rail" data-testid="shell-rail">
			<nav aria-label="Primary" class="nt-destinations nt-rail-nav">
				<a href="/home" aria-current={primaryDestination === 'home' ? 'page' : undefined}>
					Home
				</a>
				<a href="/search" aria-current={primaryDestination === 'search' ? 'page' : undefined}>
					Search
				</a>
				<a href="/library" aria-current={primaryDestination === 'library' ? 'page' : undefined}>
					Your Library
				</a>
			</nav>
			<div class="nt-chips" role="group" aria-label="Library views">
				<a href="/library">All</a>
				<a href="/library/liked">Liked</a>
				<a href="/library/recent">Recent</a>
			</div>
		</div>
		{@render children()}
	</div>
	{#if rightSnippet}
		<aside class="nt-right-panel nt-wrap" data-testid="right-panel">
			{@render rightSnippet()}
		</aside>
	{/if}
	{#if miniHint}
		<div class="nt-miniplayer nt-wrap" data-testid="mini-player">
			<a href="/library">
				<span class="nt-miniplayer-title">{miniHint.title}</span>
				{#if miniHint.artist}
					<span class="nt-miniplayer-artist">{miniHint.artist}</span>
				{/if}
				<span class="nt-miniplayer-context">Your Library</span>
			</a>
		</div>
	{/if}
	<nav aria-label="Primary" data-testid="shell-tabbar" class="nt-destinations nt-tabbar">
		<a href="/home" aria-current={primaryDestination === 'home' ? 'page' : undefined}>
			Home
		</a>
		<a href="/search" aria-current={primaryDestination === 'search' ? 'page' : undefined}>
			Search
		</a>
		<a href="/library" aria-current={primaryDestination === 'library' ? 'page' : undefined}>
			Your Library
		</a>
	</nav>
{/if}

{#if toasts.length > 0}
	<div class="nt-toasts" role="status" aria-live="polite">
		{#each toasts as toast (toast.id)}
			<div class="nt-toast" data-testid="toast">
				<span>{toast.message}</span>
				{#if toast.action}
					<button type="button" onclick={() => onToastAction(toast)}>{toast.action.label}</button>
				{/if}
			</div>
		{/each}
	</div>
{/if}

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

	.nt-brand {
		color: var(--nt-fg);
		font-weight: 700;
		text-decoration: none;
		white-space: nowrap;
	}

	.nt-public {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		justify-content: space-between;
		gap: var(--nt-space-2) var(--nt-space-4);
		padding-block: var(--nt-space-2);
		border-bottom: 1px solid var(--nt-border);
	}

	.nt-public-links {
		display: flex;
		align-items: center;
		gap: var(--nt-space-2) var(--nt-space-3);
	}

	.nt-public-links a {
		color: var(--nt-fg-muted);
		text-decoration: none;
	}

	.nt-topbar {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: var(--nt-space-2) var(--nt-space-3);
		padding-block: var(--nt-space-2);
		border-bottom: 1px solid var(--nt-border);
		background: var(--nt-bg);
	}

	.nt-topbar-search {
		flex: 1 1 8rem;
		min-width: 0;
	}

	.nt-topbar-search input {
		width: 100%;
		padding: var(--nt-space-1) var(--nt-space-2);
		border: 1px solid var(--nt-border);
		border-radius: var(--nt-radius);
		background: var(--nt-bg-raised);
		color: var(--nt-fg);
	}

	.nt-appearance {
		display: flex;
		align-items: center;
		gap: var(--nt-space-1);
		color: var(--nt-fg-muted);
	}

	.nt-appearance select {
		padding: var(--nt-space-1) var(--nt-space-2);
		border: 1px solid var(--nt-border);
		border-radius: var(--nt-radius);
		background: var(--nt-bg-raised);
		color: var(--nt-fg);
	}

	.nt-body {
		display: flex;
		align-items: flex-start;
	}

	.nt-rail {
		display: flex;
		flex: 0 0 12rem;
		flex-direction: column;
		gap: var(--nt-space-3);
		padding: var(--nt-space-3) var(--nt-space-2);
		border-right: 1px solid var(--nt-border);
	}

	/* Design-token styling pin (NAV-18): this rule matches each Primary
	 * nav itself while carrying a var(--nt-*) declaration. */
	nav.nt-destinations {
		background-color: var(--nt-bg);
	}

	.nt-rail-nav {
		display: flex;
		flex-direction: column;
		gap: var(--nt-space-1);
	}

	.nt-rail-nav a {
		padding: var(--nt-space-1) var(--nt-space-2);
		border-radius: var(--nt-radius);
		color: var(--nt-fg-muted);
		text-decoration: none;
	}

	.nt-rail-nav a[aria-current='page'],
	.nt-tabbar a[aria-current='page'] {
		color: var(--nt-fg);
		font-weight: 600;
		text-decoration: underline;
	}

	.nt-menu-root {
		position: relative;
	}

	.nt-menu-root > button {
		padding: var(--nt-space-1) var(--nt-space-2);
		border: 1px solid var(--nt-border);
		border-radius: var(--nt-radius);
		background: var(--nt-bg-raised);
		color: var(--nt-fg);
	}

	.nt-menu {
		position: absolute;
		top: calc(100% + var(--nt-space-1));
		inset-inline-start: 0;
		z-index: 20;
		display: flex;
		flex-direction: column;
		min-width: 12rem;
		padding: var(--nt-space-1);
		border: 1px solid var(--nt-border);
		border-radius: var(--nt-radius);
		background: var(--nt-bg-raised);
		color: var(--nt-fg);
	}

	.nt-menu [role='menuitem'] {
		padding: var(--nt-space-2);
		border: 0;
		border-radius: var(--nt-radius);
		background: transparent;
		color: var(--nt-fg);
		text-align: start;
		text-decoration: none;
		font: inherit;
	}

	.nt-menu [role='menuitem'][aria-current='page'] {
		font-weight: 600;
		text-decoration: underline;
	}

	.nt-chips {
		display: flex;
		flex-wrap: wrap;
		gap: var(--nt-space-1) var(--nt-space-2);
	}

	.nt-chips a {
		padding: var(--nt-space-1) var(--nt-space-2);
		border: 1px solid var(--nt-border);
		border-radius: var(--nt-radius);
		color: var(--nt-fg-muted);
		text-decoration: none;
	}

	.nt-tabbar {
		display: none;
	}

	.nt-miniplayer {
		display: none;
	}

	.nt-toasts {
		position: fixed;
		inset-inline-end: var(--nt-space-3);
		bottom: var(--nt-space-3);
		z-index: 30;
		display: flex;
		flex-direction: column;
		gap: var(--nt-space-2);
		max-width: min(24rem, calc(100vw - 2 * var(--nt-space-3)));
	}

	.nt-toast {
		display: flex;
		align-items: center;
		gap: var(--nt-space-2) var(--nt-space-3);
		padding: var(--nt-space-2) var(--nt-space-3);
		border: 1px solid var(--nt-border);
		border-radius: var(--nt-radius);
		background: var(--nt-bg-raised);
		color: var(--nt-fg);
	}

	.nt-toast button {
		padding: var(--nt-space-1) var(--nt-space-2);
		border: 1px solid var(--nt-border);
		border-radius: var(--nt-radius);
		background: var(--nt-bg);
		color: var(--nt-fg);
		white-space: nowrap;
	}

	.nt-right-panel {
		border-top: 1px solid var(--nt-border);
		padding-block: var(--nt-space-3);
	}

	.nt-miniplayer a {
		display: flex;
		align-items: baseline;
		gap: var(--nt-space-1) var(--nt-space-2);
		color: var(--nt-fg);
		text-decoration: none;
	}

	.nt-miniplayer-title {
		font-weight: 600;
	}

	.nt-miniplayer-artist,
	.nt-miniplayer-context {
		color: var(--nt-fg-muted);
	}

	@media (max-width: 799px) {
		.nt-rail {
			display: none;
		}

		.nt-body {
			display: block;
		}

		.nt-miniplayer {
			display: block;
			padding: var(--nt-space-2) var(--nt-space-3);
			border-top: 1px solid var(--nt-border);
			background: var(--nt-bg-raised);
		}

		.nt-tabbar {
			position: sticky;
			bottom: 0;
			display: flex;
			align-items: stretch;
			border-top: 1px solid var(--nt-border);
			background: var(--nt-bg-raised);
		}

		.nt-tabbar a {
			display: flex;
			flex: 1 1 0;
			align-items: center;
			justify-content: center;
			min-height: 48px;
			padding: var(--nt-space-1);
			color: var(--nt-fg-muted);
			text-decoration: none;
			text-align: center;
		}

		.nt-toasts {
			bottom: calc(48px + var(--nt-space-3));
		}
	}
</style>
