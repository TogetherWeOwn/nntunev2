<script lang="ts">
	import { onDestroy } from 'svelte';
	import { invalidateAll } from '$app/navigation';
	import type { AccountSession } from '#lib/auth-fence.js';
	import { BillingApiError } from '#lib/billing/billingClient.js';
	import { getPlan, planFromDTO, type Plan } from '#lib/billing/plans.js';
	import { evaluateUsage, formatResetInstant } from '#lib/billing/usage.js';
	import {
		markUsageWarningsShown,
		pendingUsageWarnings
	} from '#lib/billing/usageWarnings.js';
	import type { PageData } from './$types';

	let { data }: { data: PageData } = $props();
	let retired = $state.raw<AccountSession>();
	const accountEnded = $derived(!!data.owner && (retired === data.owner || !data.owner.isCurrent()));

	$effect.pre(() => {
		const owner = data.owner;
		if (!owner) return;
		const clear = () => {
			retired = owner;
			warningThreshold = null;
			actionError = null;
			busy = null;
		};
		if (!owner.isCurrent()) clear();
		owner.signal.addEventListener('abort', clear, { once: true });
		return () => owner.signal.removeEventListener('abort', clear);
	});

	// Caps render from plan data, not only the static mirror: hydrate the
	// loader's plan from GET /v1/plans (same merge as the pricing page) so
	// a plans-table change reaches the meter and warnings without a client
	// release. The static row stays as the fallback while the read is in
	// flight or when it fails — display only either way, enforcement is
	// the server's. A stale hydration from an earlier load never applies
	// to a different plan after a reload swaps `data.planId`, and the
	// `loadSeq` read below re-runs this effect on *every* loader run —
	// including a Retry `invalidateAll()` with an unchanged planId — so a
	// cap changed server-side since the last load is always refetched
	// before the warning check may score (and permanently mark) against it.
	let hydratedPlan = $state<Plan | null>(null);
	let planHydrationDone = $state(false);
	$effect(() => {
		void data.loadSeq;
		const planId = data.planId;
		planHydrationDone = false;
		let cancelled = false;
		(async () => {
			try {
				const { fetchPlans } = await import('#lib/billing/billingClient.js');
				const dtos = await fetchPlans();
				if (cancelled) return;
				const dto = dtos.find((d) => d.id === planId);
				const merged = dto ? planFromDTO(dto) : undefined;
				if (merged) hydratedPlan = merged;
			} catch {
				/* static mirror stays */
			} finally {
				if (!cancelled) planHydrationDone = true;
			}
		})();
		return () => {
			cancelled = true;
		};
	});
	const livePlan = $derived(
		hydratedPlan !== null && hydratedPlan.id === data.planId ? hydratedPlan : null
	);

	// The loader only emits known plan ids; free is the last-resort fallback
	// so the page can never promote an unknown state to a paid tier.
	const plan = $derived(livePlan ?? getPlan(data.planId) ?? getPlan('free')!);
	// A lapsed subscriber still owns their plan: name it with its status
	// instead of printing "No plan" above a past_due line. The
	// "couldn't confirm" wording is reserved for no-subscription-at-all.
	const headingPlan = $derived(
		!data.planUnknown || data.signedOut
			? plan
			: data.subscription?.planId
				? (getPlan(data.subscription.planId) ?? plan)
				: plan
	);
	// usageStatus is null unless the plan is confirmed AND usage loaded: no
	// usage-vs-cap alert or upgrade button is rendered otherwise. Once the
	// plan hydrates, usage re-scores against the hydrated cap (keeping the
	// loader's portal-only upgrade suppression), so the meter, warnings
	// and upgrade copy never quote a superseded mirror cap.
	const status = $derived.by(() => {
		if (livePlan === null || !data.usage || !data.usageStatus) return data.usageStatus;
		const scored = evaluateUsage(livePlan, data.usage);
		return data.manageBillingOnly ? { ...scored, upgradeTo: null } : scored;
	});

	// 80% / 95% warnings fire once per threshold per month (spec §3.2):
	// pending thresholds are read from localStorage, the highest is shown,
	// and all of them are marked shown so a lower one never fires late.
	// The check waits for the plan-data read to settle so a threshold is
	// never scored (and permanently marked) against a stale mirror cap.
	let warningThreshold = $state<number | null>(null);
	$effect(() => {
		if (!data.owner?.isCurrent() || !planHydrationDone || !data.usage || !status) return;
		const pending = pendingUsageWarnings(data.usage.period, status.streamFraction);
		if (pending.length === 0) return;
		warningThreshold = pending[pending.length - 1];
		markUsageWarningsShown(data.usage.period, pending);
	});

	let actionError: string | null = $state(null);
	let busy: 'checkout' | 'portal' | null = $state(null);
	let alive = true;
	onDestroy(() => {
		alive = false;
	});

	async function billingAction(action: 'checkout' | 'portal') {
		// Belt and braces with the loader (which nulls upgradeTo for
		// portal-only subscriptions): never start a checkout from here when
		// the subscriber already holds a plan that needs the portal instead.
		const owner = data.owner, upgrade = status?.upgradeTo;
		if (!alive || busy || !owner?.isCurrent() || (action === 'checkout' && (!upgrade || data.manageBillingOnly))) return;
		const current = () => alive && owner === data.owner && owner.isCurrent();
		actionError = null;
		busy = action;
		try {
			const { startCheckout, openPortal } = await import('#lib/billing/billingClient.js');
			const sent = await owner.run(() => current() ? {
				result: (action === 'checkout' && upgrade ? startCheckout(upgrade, fetch, owner) : openPortal(fetch, owner))
					.then((url) => ({ url }), (cause: unknown) => ({ cause }))
			} : undefined);
			const result = await sent?.result;
			if (!current()) return;
			if (!result) throw new Error('Could not verify your billing session.');
			if ('cause' in result) throw result.cause;
			window.location.href = result.url;
		} catch (err) {
			if (!current()) return;
			if (!(err instanceof BillingApiError && err.paywall)) {
				actionError = err instanceof Error ? err.message : action === 'checkout' ? 'Checkout failed' : 'Portal failed';
			}
			busy = null;
		} finally {
			if (!alive) busy = null;
		}
	}
</script>

<svelte:head>
	<title>Billing — NNTune v2</title>
</svelte:head>

<main>
	<h1>Billing</h1>

	{#if accountEnded}
		<p role="status">Your session changed. Retry to see your current billing details.</p>
		<button onclick={() => invalidateAll()}>Retry</button>
	{:else}
	{#if data.loadError}
		<p role="alert">{data.loadError}</p>
	{/if}

	{#if data.planUnknown && !data.signedOut}
		<p role="status">We couldn&apos;t confirm an active plan — showing no-plan defaults.</p>
	{/if}

	{#if data.signedOut}
		<p><a href="/login">Sign in</a> to see your billing.</p>
	{/if}

	{#if data.hadFetchError}
		<button onclick={() => invalidateAll()}>Retry</button>
	{/if}

	<section aria-labelledby="plan">
		<h2 id="plan">Current plan: {headingPlan.name}</h2>
		{#if data.subscription?.status}
			{@const periodEnd = data.subscription.currentPeriodEnd}
			<p>
				Status: {data.subscription.status}
				{#if periodEnd}
					{@const displayDate = new Date(periodEnd).toLocaleDateString()}
					<!-- HTML datetime requires a positive year and at most millisecond precision. -->
					{#if periodEnd.startsWith('0000-')}
						· period ends {displayDate}
					{:else}
						<!-- HTML also requires a nonnegative sign for a zero offset. -->
						{@const datetime = periodEnd.replace(/(\.\d{3})\d+/, '$1').replace(/-00:?00$/, 'Z')}
						· period ends <time {datetime}>{displayDate}</time>
					{/if}
				{/if}
				{#if data.subscription.cancelAtPeriodEnd}
					· cancels at period end
				{/if}
			</p>
		{/if}
	</section>

	{#if data.usage && status}
		<section aria-labelledby="usage" aria-live="polite">
			<h2 id="usage">Usage for {data.usage.period}</h2>
			<ul>
				<li>
					Streaming: {(data.usage.streamSecondsMonth / 3600).toFixed(1)} / {plan.streamHoursMonth} hrs
					({Math.round(status.streamFraction * 100)}%)
					<progress
						aria-label="Monthly streaming usage"
						aria-valuetext={`${(data.usage.streamSecondsMonth / 3600).toFixed(1)} of ${plan.streamHoursMonth} hours used this month`}
						max={plan.streamHoursMonth * 3600}
						value={Math.min(data.usage.streamSecondsMonth, plan.streamHoursMonth * 3600)}
					>
						{Math.round(status.streamFraction * 100)}%
					</progress>
				</li>
			</ul>
			<p>{formatResetInstant(data.usage.resetsAt)}</p>

			{#if warningThreshold !== null}
				<p role="status">
					You&apos;ve crossed {warningThreshold}% of your {plan.name} listening hours this month.
				</p>
			{/if}

			{#if status.state !== 'ok'}
				<div role={status.state === 'hard' ? 'alert' : 'status'}>
					<p><strong>{status.headline}</strong></p>
					{#if status.detail}<p>{status.detail}</p>{/if}
				</div>
			{/if}
		</section>
	{/if}

	{#if actionError}
		<p role="alert">{actionError}</p>
	{/if}

	{#if data.owner && !data.signedOut}
		<div>
			{#if status?.upgradeTo && !data.manageBillingOnly}
				<button disabled={busy !== null} onclick={() => billingAction('checkout')}>
					{busy === 'checkout' ? 'Starting…' : `Upgrade to ${status.upgradeTo}`}
				</button>
			{/if}
			{#if data.subscription?.state === 'no_subscription'}
				<!-- Never-subscribed: the purchase route is the primary action.
				     A portal button would be a dead end — there is no billing
				     record to manage yet. -->
				<a href="/pricing">See plans</a>
			{:else}
				<button disabled={busy !== null} onclick={() => billingAction('portal')}>
					{busy === 'portal'
						? 'Opening…'
						: data.manageBillingOnly
							? 'Manage billing to resolve'
							: 'Manage billing'}
				</button>
			{/if}
		</div>
	{/if}
	{/if}

	<p><a href="/pricing">Compare all plans</a></p>
</main>

<style>
	main {
		max-width: 44rem;
		margin: 2rem auto;
		padding: 0 1rem;
		font-family: system-ui, sans-serif;
	}
	progress {
		width: 100%;
	}
	div:has(button) {
		display: flex;
		gap: 0.75rem;
		margin-top: 1rem;
	}
</style>
