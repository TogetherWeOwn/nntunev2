<script lang="ts">
	import { onDestroy, onMount } from 'svelte';
	import { goto } from '$app/navigation';
	import {
		listSelectablePlans,
		planFromDTO,
		formatPricePerMonth,
		planButtonName,
		type Plan
	} from '#lib/billing/plans.js';
	import {
		BillingApiError,
		fetchPlans,
		startCheckout,
		signupUrlForCheckoutFailure
	} from '#lib/billing/billingClient.js';

	// Static fallback mirror first (signed-out visitors have no session for
	// GET /v1/plans); signed-in visitors hydrate from plan data on mount so
	// prices/caps render from the plans table, never a second copy here.
	let plans = $state<Plan[]>(listSelectablePlans());
	// The server-rendered buttons cannot start checkout until this component
	// mounts, so keep them disabled instead of silently dropping an early click.
	let mounted = $state(false);
	let checkoutError: string | null = $state(null);
	let pendingPlan: string | null = $state(null);
	let alive = true;
	onDestroy(() => {
		alive = false;
	});

	onMount(async () => {
		mounted = true;
		try {
			const dtos = await fetchPlans();
			if (!alive) return;
			const hydrated = dtos
				.map((dto) => planFromDTO(dto))
				.filter((plan): plan is Plan => plan !== undefined && plan.selectable);
			// An empty or unrecognized response never blanks the page: the
			// static fallback mirror stays.
			if (hydrated.length > 0) plans = hydrated;
		} catch {
			/* signed-out 401 or backend unreachable: static fallback stays */
		}
	});

	async function choose(planId: string) {
		// Serialize plan selections: ignore any activation while a checkout
		// request is already in flight (defense in depth alongside the
		// disabled buttons below).
		if (!alive || pendingPlan !== null) return;
		checkoutError = null;
		pendingPlan = planId;
		try {
			const url = await startCheckout(planId);
			if (!alive) return;
			window.location.href = url;
			// Keep pendingPlan set on success: assigning location.href only
			// starts the cross-origin navigation, and the page stays
			// interactive until the checkout host responds. Clearing the
			// lock here would let a second activation issue another
			// checkout whose navigation replaces this one.
		} catch (err) {
			if (!alive) return;
			try {
				// Signed-out visitors have no session cookie: send them to signup
				// with the plan carried along instead of a raw 401 alert.
				const signup = signupUrlForCheckoutFailure(planId, err);
				if (signup) await goto(signup);
				else if (!(err instanceof BillingApiError && err.paywall))
					checkoutError = err instanceof Error ? err.message : 'Checkout failed';
			} finally {
				pendingPlan = null;
			}
		} finally {
			if (!alive) pendingPlan = null;
		}
	}
</script>

<svelte:head>
	<title>Plans and pricing — NNTune v2</title>
	<meta
		name="description"
		content="NNTune v2 plans: Plus, Pro and Max. Every plan includes the full catalogue and lossless audio — plans differ in monthly listening hours. No overage bills, ever."
	/>
</svelte:head>

<main>
	<h1>Plans and pricing</h1>
	<p>
		NNTune is an invite-only, curated listening service. Every plan includes the full
		catalogue and lossless audio — plans differ only in how many hours a month you can
		listen. Browsing, search and your library are always available; a plan is what turns
		on playback.
	</p>

	{#if checkoutError}
		<p role="alert">{checkoutError}</p>
	{/if}

	<table>
		<caption>Monthly plans. Every plan includes the full catalogue; plans differ in listening hours.</caption>
		<thead>
			<tr>
				<th scope="col">Plan</th>
				<th scope="col">Price</th>
				<th scope="col">Stream hrs/mo</th>
				<th scope="col">Concurrent</th>
				<th scope="col"><span class="visually-hidden">Action</span></th>
			</tr>
		</thead>
		<tbody>
			{#each plans as plan (plan.id)}
				<tr>
					<th scope="row">{plan.name}<br /><small>{plan.blurb}</small></th>
					<!-- Trailing {' '} keeps cell values separated in textContent (Svelte
						strips inter-element whitespace, so without it text extraction
						jams cells and word-boundary matching on the values fails).
						Rendered output is unchanged: HTML collapses the spaces. -->
					<td>{formatPricePerMonth(plan)}{' '}</td>
					<td>{plan.streamHoursMonth}{' '}</td>
					<td>{plan.maxConcurrentStreams}{' '}</td>
					<td>
						<button
							aria-label={planButtonName(plan, pendingPlan)}
							disabled={!mounted || pendingPlan !== null}
							onclick={() => choose(plan.id)}
						>
							{pendingPlan === plan.id ? 'Starting…' : 'Choose'}
						</button>
					</td>
				</tr>
			{/each}
		</tbody>
	</table>

	<section aria-labelledby="cap">
		<h2 id="cap">What happens at the cap</h2>
		<p>
			No overage bills, ever. When you reach your monthly hours, new playback pauses
			until your hours reset (the 1st, 00:00 UTC) or you upgrade — a track already
			playing finishes. Every plan plays one stream at a time; starting playback on a
			second device hands over from the first.
		</p>
		<p><a href="/account/billing">Check your current usage</a></p>
	</section>
</main>

<style>
	main {
		max-width: 60rem;
		margin: 2rem auto;
		padding: 0 1rem;
		font-family: system-ui, sans-serif;
	}
	table {
		width: 100%;
		border-collapse: collapse;
		margin: 1.5rem 0;
	}
	th,
	td {
		border: 1px solid #ccc;
		padding: 0.5rem 0.6rem;
		text-align: left;
		vertical-align: top;
	}
	small {
		font-weight: normal;
		color: var(--nt-fg-muted);
	}
	.visually-hidden {
		position: absolute;
		clip: rect(0 0 0 0);
	}
</style>
