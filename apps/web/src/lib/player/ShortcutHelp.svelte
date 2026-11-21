<script lang="ts">
	import { tick } from 'svelte';
	import { actionForKey, SHORTCUT_ROWS, type ShortcutAction } from './shortcuts.js';

	let { actions, buttonClass }: { actions?: readonly ShortcutAction[]; buttonClass?: string } = $props();
	const rows = $derived(SHORTCUT_ROWS.filter((row) => {
		const action = actionForKey(row.key);
		return !actions || (action !== null && actions.includes(action));
	}));
	let opened = $state(false);
	let dialog: HTMLDialogElement | undefined = $state();
	let button: HTMLButtonElement;
	let opener: HTMLElement | null = null;

	export function isOpen(): boolean {
		return opened;
	}

	export async function open(from?: HTMLButtonElement): Promise<void> {
		if (opened) return;
		opener = from ?? (document.activeElement instanceof HTMLElement ? document.activeElement : null);
		opened = true;
		await tick();
		if (opened) dialog?.showModal();
	}

	export function close(): boolean {
		if (!opened) return false;
		dialog?.close();
		opened = false;
		(opener?.isConnected ? opener : button).focus();
		opener = null;
		return true;
	}

	function trapTab(event: KeyboardEvent): void {
		if (event.key !== 'Tab' || event.defaultPrevented || event.metaKey || event.ctrlKey || event.altKey) return;
		// This dialog has one focusable control. Native modality alone can send
		// focus to the browser chrome at either end of the tab order.
		event.preventDefault();
		dialog?.querySelector('button')?.focus();
	}
</script>

<button bind:this={button} type="button" class={buttonClass} data-testid="shortcuts-button"
	aria-haspopup="dialog" onclick={(event) => void open(event.currentTarget)}>
	Keyboard shortcuts
</button>

{#if opened}
	<dialog bind:this={dialog} data-testid="shortcuts-dialog" aria-label="Keyboard shortcuts"
		onkeydown={trapTab} oncancel={(event) => { event.preventDefault(); close(); }}>
		<h2>Keyboard shortcuts</h2>
		<ul>
			{#each rows as row (row.key)}
				<li><kbd>{row.key}</kbd> {row.label}</li>
			{/each}
		</ul>
		<button type="button" data-testid="shortcuts-close" onclick={close}>Close</button>
	</dialog>
{/if}
