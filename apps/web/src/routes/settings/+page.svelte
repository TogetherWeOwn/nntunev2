<script lang="ts">
	import ListenBrainzSection from '#lib/listenbrainz/ListenBrainzSection.svelte';
</script>

<svelte:head>
	<title>Settings — NNTune</title>
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

	<ListenBrainzSection />

	<section aria-labelledby="settings-account-heading">
		<h2 id="settings-account-heading">Account</h2>
		<p>Plan, provider and sign-in settings live in your account.</p>
		<ul>
			<li><a href="/account/billing">Open Account</a></li>
		</ul>
	</section>
</main>
