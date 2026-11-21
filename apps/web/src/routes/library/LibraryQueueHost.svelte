<script lang="ts">
	import { onMount, tick, type Snippet } from 'svelte';
	import { readRightPanel, setRightPanel } from '#lib/shell/right-panel.js';

	let { children }: { children: Snippet } = $props();

	let wide = $state(false);
	let expanded = $state(false);
	let toggle: HTMLButtonElement;
	let closeButton: HTMLButtonElement;
	let panel: HTMLDivElement;
	let inline: HTMLDivElement;
	let projection: HTMLDivElement;

	// Move the route-owned subtree as a unit: re-rendering the snippet in a
	// second host would reset queue selection, confirmation and copy feedback.
	function receive(node: HTMLDivElement) {
		const content = projection;
		const fallback = inline;
		node.append(content);
		return {
			destroy() {
				if (content.parentElement === node && fallback.isConnected) fallback.append(content);
			}
		};
	}

	function releaseSlot(): void {
		if (readRightPanel() === queuePanel) setRightPanel(null);
	}

	function close(restoreToggle: boolean): void {
		const active = document.activeElement;
		const focusInPanel = panel?.contains(active);
		if (projection.parentElement !== inline) inline.append(projection);
		expanded = false;
		releaseSlot();
		if (restoreToggle) toggle.focus();
		else if (focusInPanel || active === toggle) {
			if (active instanceof HTMLElement && projection.contains(active)) active.focus();
			else projection.focus();
		}
	}

	async function togglePanel(): Promise<void> {
		if (expanded) return close(true);
		if (!wide || readRightPanel()) return;
		expanded = true;
		setRightPanel(queuePanel);
		await tick();
		if (!expanded || readRightPanel() !== queuePanel) return;
		if (closeButton?.isConnected) closeButton.focus();
		else close(false);
	}

	function escape(event: KeyboardEvent): void {
		if (event.key !== 'Escape' || event.defaultPrevented || !expanded) return;
		if (!(event.target instanceof Node) || !panel?.contains(event.target)) return;
		event.preventDefault();
		close(true);
	}

	onMount(() => {
		// Match the shell's breakpoint, with inline as the safe initial host.
		const narrow = window.matchMedia('(max-width: 799px)');
		const resize = () => {
			if (narrow.matches && (expanded || document.activeElement === toggle)) close(false);
			wide = !narrow.matches;
		};
		resize();
		narrow.addEventListener('change', resize);
		return () => {
			narrow.removeEventListener('change', resize);
			releaseSlot();
		};
	});
</script>

<svelte:window onkeydown={escape} />

<button
	type="button"
	class="nt-button"
	hidden={!wide}
	bind:this={toggle}
	aria-expanded={expanded}
	aria-controls={expanded ? 'library-queue-panel' : undefined}
	onclick={togglePanel}
>
	Queue in side panel
</button>
<div bind:this={inline}>
	<div bind:this={projection} tabindex="-1">
		{@render children()}
	</div>
</div>

{#snippet queuePanel()}
	<div id="library-queue-panel" bind:this={panel}>
		<button type="button" class="nt-button" bind:this={closeButton} onclick={() => close(true)}>
			Close queue panel
		</button>
		<div use:receive></div>
	</div>
{/snippet}

<style>
	.nt-button[hidden] { display: none; }
</style>
