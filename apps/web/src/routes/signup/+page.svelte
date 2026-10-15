<script lang="ts">
	// Signup form shell (TOG-25176). The hooks below are the e2e contract
	// (e2e/helpers/contract.ts TESTIDS); account creation itself lands with
	// the auth slice, which wires this form to the real backend. Until then
	// submit explains that honestly instead of faking a success.
	let email = $state('');
	let error: string | null = $state(null);

	function signup(event: SubmitEvent) {
		event.preventDefault();
		error =
			'Sign-up is not available in this build yet — account creation lands with the auth slice.';
	}
</script>

<svelte:head>
	<title>NNTune — get started</title>
	<meta name="description" content="Create your NNTune account and connect your Usenet provider." />
</svelte:head>

<main class="nt-wrap">
	<h1>Get started</h1>
	<p>
		Sign-up with passkeys and the bring-your-own-provider wizard lands in the next slice. This
		placeholder keeps the landing call-to-action working until then.
	</p>
	<form onsubmit={signup}>
		<label>
			Email
			<input data-testid="signup-email" type="email" name="email" required bind:value={email} />
		</label>
		<button data-testid="signup-submit" type="submit">Create account</button>
	</form>
	{#if error}
		<p data-testid="signup-error" role="alert">{error}</p>
	{/if}
</main>
