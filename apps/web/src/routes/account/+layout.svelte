<script lang="ts">
	import { page } from '$app/state';
	import type { LayoutProps } from './$types';

	let { children }: LayoutProps = $props();
</script>

<div class="nt-wrap account-shell">
	<nav aria-label="Account">
		<a
			href="/account/billing"
			aria-current={page.url.pathname === '/account/billing' ? 'page' : undefined}
		>Billing</a>
		<a
			href="/account/security"
			aria-current={page.url.pathname === '/account/security' ? 'page' : undefined}
		>Security</a>
	</nav>

	{@render children()}
</div>

<style>
	.account-shell {
		max-width: 46rem;
		margin-block: var(--nt-space-5);
	}

	nav {
		display: flex;
		gap: var(--nt-space-3);
		margin-bottom: var(--nt-space-4);
	}
</style>
