<script lang="ts">
	import {
		attributionInventory,
		validateAttributionInventory,
		type Attribution
	} from '#lib/attributions/index.js';
	import { APP_VERSION } from '#lib/version.js';
	import { BUILD_SHA, isBuildShaKnown } from '#lib/build.js';

	// Attribution rendering is a pure function of the shipped inventory:
	// no fetch, loading skeleton or router state. The only failure mode is a
	// failed structural validation, which renders an announced error instead
	// of a partial list. Headings and list spacing follow the existing
	// Settings sections (h2 + paragraph + ul); no new token or component.
	const validationErrors = validateAttributionInventory(attributionInventory);
	const active = attributionInventory.entries.filter((entry) => entry.status === 'active');
	const planned = attributionInventory.entries.filter((entry) => entry.status === 'planned');
	const licenceEntries = active.filter((entry) => entry.kind !== 'data' && entry.kind !== 'artwork');
	const dataEntries = active.filter((entry) => entry.kind === 'data' || entry.kind === 'artwork');

	// Machine-readable provenance: the exact serialized inventory the lists
	// above render from, embedded verbatim for consumers that parse the page.
	// `<` is escaped so the payload can never close the script element early.
	const attributionJson = JSON.stringify(attributionInventory).replaceAll('<', '\\u003c');

	function licenceText(license: string): string {
		if (license === 'MIT') return 'MIT License';
		if (license === 'OFL-1.1') return 'Open Font License';
		return 'Licence';
	}
</script>

<svelte:head>
	<title>Settings — NNTune</title>
	<meta name="nntune:attributions:schema" content="1" />
	<script type="application/json" id="nntune-attribution-inventory">{@html attributionJson}</script>
</svelte:head>

<main class="nt-wrap" id="main-content">
	<h1>Settings</h1>

	<section aria-labelledby="settings-library-heading">
		<h2 id="settings-library-heading">Your Library</h2>
		<p>Your library, favorites and recent searches stay on this device and are never uploaded.</p>
		<ul>
			<li><a href="/library">Open Your Library</a></li>
			<li><a href="/library/liked">Open Liked</a></li>
			<li><a href="/library/recent">Open Recent searches</a></li>
		</ul>
	</section>

	<section aria-labelledby="settings-account-heading">
		<h2 id="settings-account-heading">Account</h2>
		<p>Plan, provider and sign-in settings live in your account.</p>
		<ul>
			<li><a href="/account/billing">Open Account</a></li>
		</ul>
	</section>

	<section aria-labelledby="settings-about-heading" id="about-nntune">
		<h2 id="settings-about-heading">About NNTune</h2>
		<p>Version {APP_VERSION}{isBuildShaKnown(BUILD_SHA) ? ` · build ${BUILD_SHA.slice(0, 7)}` : ''}</p>

		{#if validationErrors.length > 0}
			<p role="alert">Attribution data is unavailable.</p>
		{:else}
			<h3 id="settings-about-licences-heading">Licences</h3>
			<p>NNTune is open source under the MIT License.</p>
			<p>Typeface: Figtree, SIL Open Font License 1.1.</p>
			<ul>
				{#each licenceEntries as entry (entry.id)}
					{@render attributionEntry(entry)}
				{/each}
			</ul>

			{#if dataEntries.length > 0}
				<h3 id="settings-about-sources-heading">Data sources</h3>
				<ul>
					{#each dataEntries as entry (entry.id)}
						{@render attributionEntry(entry)}
					{/each}
				</ul>
			{/if}

			{#if planned.length > 0}
				<h3 id="settings-about-planned-heading">Planned sources</h3>
				<ul>
					{#each planned as entry (entry.id)}
						{@render attributionEntry(entry)}
					{/each}
				</ul>
			{/if}
		{/if}
	</section>

	{#snippet attributionEntry(entry: Attribution)}
		<li data-attribution-id={entry.id} data-status={entry.status}>
			<strong>{entry.name}</strong> ({entry.license}{entry.package?.version ? ` ${entry.package.version}` : ''})
			<p>{entry.notice}</p>
			<p>{entry.obligations}</p>
			<p>
				<a href={entry.sourceUrl}>Source</a>
				{' · '}
				<a href={entry.licenseUrl}>{licenceText(entry.license)}</a>
				{#if entry.id === 'figtree'}
					{' · '}
					<a href="/fonts/OFL.txt">Open Font License (shipped copy)</a>
				{/if}
			</p>
		</li>
	{/snippet}
</main>
