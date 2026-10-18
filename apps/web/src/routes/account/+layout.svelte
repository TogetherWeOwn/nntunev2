<script lang="ts">
	import type { LayoutProps } from './$types';

	let { children }: LayoutProps = $props();
</script>

<div class="account-shell">
	<nav aria-label="Account">
		<a href="/account/billing">Billing</a>
		<a href="/account/security">Security</a>
	</nav>

	{@render children()}
</div>

<style>
	.account-shell {
		max-width: 46rem;
		margin: 2rem auto;
		padding: 0 1rem;
		font-family: system-ui, sans-serif;
	}

	nav {
		display: flex;
		gap: 1rem;
		margin-bottom: 1.5rem;
	}
</style>
