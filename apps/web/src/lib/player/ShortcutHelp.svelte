<script lang="ts">
	import { tick } from 'svelte';
	import { actionForKey, SHORTCUT_ROWS, type ShortcutAction } from './shortcuts.js';

	let { actions, buttonClass }: { actions?: readonly ShortcutAction[]; buttonClass?: string } = $props();
	const rows = $derived(SHORTCUT_ROWS.filter(({ key }) =>
		!actions || actions.some((action) => action === actionForKey(key))));
	let opened = $state(false);
	let shortcutsDialogEl: HTMLDialogElement | undefined = $state();
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
		if (opened) shortcutsDialogEl?.showModal();
	}

	export function close(): void {
		if (!opened) return;
		shortcutsDialogEl?.close();
		opened = false;
		(opener?.isConnected ? opener : button).focus();
		opener = null;
	}

	// Native modality can send focus to the browser chrome at the ends of the tab order.
	function trapShortcutsTab(event: KeyboardEvent): void {
		if (event.key !== 'Tab') return;
		if (event.defaultPrevented) return;
		if (event.metaKey || event.ctrlKey || event.altKey) return;
		const root = shortcutsDialogEl;
		if (!root) return;
		const controls = [...root.querySelectorAll<HTMLElement>(
			'a[href], button:not([disabled]), input, select, textarea, [tabindex]:not([tabindex="-1"])'
		)].filter((control) => control.getClientRects().length > 0);
		if (controls.length === 0) {
			event.preventDefault();
			return;
		}
		const first = controls[0];
		const last = controls[controls.length - 1];
		const active = document.activeElement;
		if (event.shiftKey) {
			if (active === first || !root.contains(active)) {
				event.preventDefault();
				last.focus();
			}
		} else if (active === last || !root.contains(active)) {
			event.preventDefault();
			first.focus();
		}
	}
</script>

<button bind:this={button} type="button" class={buttonClass} data-testid="shortcuts-button"
	aria-haspopup="dialog" onclick={(event) => void open(event.currentTarget)}>
	Keyboard shortcuts
</button>

{#if opened}
	<dialog bind:this={shortcutsDialogEl} data-testid="shortcuts-dialog" aria-label="Keyboard shortcuts"
		onkeydown={trapShortcutsTab} oncancel={(event) => { event.preventDefault(); close(); }}>
		<h2>Keyboard shortcuts</h2>
		<ul>
			{#each rows as row (row.key)}
				<li><kbd>{row.key}</kbd> {row.label}</li>
			{/each}
		</ul>
		<button type="button" data-testid="shortcuts-close" onclick={close}>Close</button>
	</dialog>
{/if}
