<script lang="ts">
	import { onMount, tick, type Snippet } from 'svelte';
	import { readRightPanel, setRightPanel } from '#lib/shell/right-panel.js';

	let { children, available, density, dismissMenu }: {
		children: Snippet; available: boolean; density: string; dismissMenu: () => HTMLElement | undefined;
	} = $props();

	let wide = $state(false);
	let expanded = $state(false);
	let toggle: HTMLButtonElement;
	let closeButton: HTMLButtonElement;
	let panel: HTMLDivElement;
	let inline: HTMLDivElement;
	let projection: HTMLDivElement;
	let fit: HTMLSpanElement;

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
		const invoker = dismissMenu();
		// Moving the subtree closes native popovers; recover to the menu trigger.
		const target = active?.closest('[popover]') ? invoker : active;
		if (projection.parentElement !== inline) inline.append(projection);
		expanded = false;
		releaseSlot();
		if (restoreToggle) toggle.focus();
		else if (focusInPanel || active === toggle) {
			if (target instanceof HTMLElement && projection.contains(target)) target.focus();
			if (!projection.contains(document.activeElement)) projection.focus();
	}
	}

	$effect(() => {
		if (!available && expanded) {
			expanded = false;
			releaseSlot();
		}
	});

	async function togglePanel(): Promise<void> {
		if (expanded) return close(true);
		if (!available || !wide || readRightPanel()) return;
		expanded = true;
		setRightPanel(queuePanel);
		await tick();
		if (!expanded || readRightPanel() !== queuePanel) return;
		if (closeButton?.isConnected) closeButton.focus();
		else close(false);
	}

	function escape(event: KeyboardEvent): void {
		if (event.key !== 'Escape' || event.defaultPrevented || event.isComposing || !expanded) return;
		if (!(event.target instanceof Node) || !panel?.contains(event.target)) return;
		event.preventDefault();
		close(true);
	}

	onMount(() => {
		const sizing = new ResizeObserver(() => {
			wide = fit.offsetWidth > 0;
			if (!wide && (expanded || document.activeElement === toggle)) close(false);
		});
		sizing.observe(fit);
		return () => {
			sizing.disconnect();
			releaseSlot();
		};
	});
</script>

<svelte:window onkeydown={escape} />

<span class="queue-fit" bind:this={fit}></span>
<button
	type="button"
	class="nt-button"
	hidden={!wide || !available}
	bind:this={toggle}
	aria-expanded={expanded}
	aria-controls={expanded ? 'library-queue-panel' : undefined}
	onclick={togglePanel}
>
	Queue in side panel
</button>
<div bind:this={inline}>
	<div class="nt-library-queue" data-density={density} bind:this={projection} tabindex="-1">
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
	/* Reserve 12rem for navigation, 19rem for queue and 19rem for Library. */
	.queue-fit { position: absolute; inline-size: 0; visibility: hidden; }
	@container shell (min-width: max(800px, 50rem)) {
		.queue-fit { inline-size: 1px; }
	}
	.nt-button[hidden] { display: none; }
	#library-queue-panel { overflow-wrap: anywhere; }
	#library-queue-panel :global(.queue-selection label),
	#library-queue-panel :global(.queue-selection select) { max-inline-size: 100%; }
</style>
