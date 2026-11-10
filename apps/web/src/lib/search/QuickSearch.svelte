<script lang="ts">
	import { onMount } from 'svelte';
	import SearchPanel from './SearchPanel.svelte';

	let { onclose }: { onclose: () => void } = $props();
	let dialog: HTMLDialogElement;
	onMount(() => {
		const opener = document.activeElement;
		dialog.showModal();
		return () => {
			dialog.close();
			if (opener instanceof HTMLElement && opener.isConnected && document.activeElement === document.body) opener.focus();
		};
	});
	function wrapTab(event: KeyboardEvent): void {
		if (event.key !== 'Tab') return;
		const controls = [...dialog.querySelectorAll<HTMLElement>('input, button, a[href]')]
			.filter((element) => !element.matches(':disabled') && element.getClientRects().length > 0);
		const first = controls[0];
		const last = controls[controls.length - 1];
		if (event.shiftKey && document.activeElement === first) { event.preventDefault(); last?.focus(); }
		else if (!event.shiftKey && document.activeElement === last) { event.preventDefault(); first?.focus(); }
	}
</script>

<dialog bind:this={dialog} aria-labelledby="quick-search-title" onclose={onclose} onkeydown={wrapTab}>
	<h2 id="quick-search-title">Quick search</h2>
	<SearchPanel compact onselect={() => dialog.close()} />
	<button type="button" class="nt-button nt-button--ghost" onclick={() => dialog.close()}>Close quick search</button>
</dialog>

<style>
	dialog { width: min(100% - var(--nt-space-4), 40rem); max-height: calc(100dvh - var(--nt-space-5)); overflow-y: auto; padding: var(--nt-space-3); border: 1px solid var(--nt-border); border-radius: var(--nt-radius); background: var(--nt-bg); color: var(--nt-fg); box-shadow: var(--nt-shadow-overlay); }
	dialog::backdrop { background: var(--nt-scrim); }
</style>
