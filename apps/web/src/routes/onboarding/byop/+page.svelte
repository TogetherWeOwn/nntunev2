<script lang="ts">
	// BYOP wizard shell (TOG-25176). The hooks below are the e2e contract
	// (e2e/helpers/contract.ts TESTIDS); connection testing and saving land
	// with TOG-23633, which wires this form to the real backend. Until then
	// both actions explain that honestly instead of faking a success.
	let host = $state('');
	let port = $state('563');
	let user = $state('');
	let pass = $state('');
	let tls = $state(true);
	let status: string | null = $state(null);

	function testConnection(event: Event) {
		event.preventDefault();
		status =
			'Connection testing lands with the BYOP wizard slice (TOG-23633) — settings are not saved yet.';
	}

	function save(event: Event) {
		event.preventDefault();
		status =
			'Saving lands with the BYOP wizard slice (TOG-23633) — settings are not saved yet.';
	}
</script>

<svelte:head>
	<title>NNTune — connect your provider</title>
	<meta
		name="description"
		content="Point NNTune at your own Usenet provider. NNTune never bundles or resells provider access."
	/>
</svelte:head>

<main class="nt-wrap">
	<h1>Connect your provider</h1>
	<p>
		NNTune never bundles Usenet access — you bring your own provider account, and we provide
		the music experience on top.
	</p>
	<form>
		<label>
			Host
			<input data-testid="byop-host" name="host" required bind:value={host} />
		</label>
		<label>
			Port
			<input data-testid="byop-port" name="port" inputmode="numeric" required bind:value={port} />
		</label>
		<label>
			Username
			<input data-testid="byop-user" name="user" autocomplete="username" bind:value={user} />
		</label>
		<label>
			Password
			<input
				data-testid="byop-pass"
				type="password"
				name="pass"
				autocomplete="current-password"
				bind:value={pass}
			/>
		</label>
		<label>
			<input data-testid="byop-tls" type="checkbox" name="tls" bind:checked={tls} />
			Use TLS
		</label>
		<button data-testid="byop-test-connection" type="button" onclick={testConnection}>
			Test connection
		</button>
		<button data-testid="byop-save" type="button" onclick={save}>Save</button>
	</form>
	{#if status}
		<p data-testid="byop-status" role="status">{status}</p>
	{/if}
</main>
