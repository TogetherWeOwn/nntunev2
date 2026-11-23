<script lang="ts">
	import { onMount, tick } from 'svelte';
	import { listen, stopListening } from './listen.js';
	import { inlineSingleActions, menuPosition, type MenuAction, type MenuRequest } from './context-menu.js';
	import ContextMenu from './ContextMenu.svelte';

	let { request, onclose, onactivate, submenu = false }: {
		request: MenuRequest;
		onclose: () => void;
		onactivate?: () => void;
		submenu?: boolean;
	} = $props();
	let menu: HTMLDivElement;
	let opening: MenuRequest;
	let active = $state(0);
	let ready = $state(false);
	let sheet = $state(false);
	let prefix = '';
	let typedAt = 0;
	let closed = false;
	let child = $state<MenuRequest | null>(null);
	let hovered: EventTarget | null = null;
	let hover: ReturnType<typeof setTimeout> | undefined;
	let menuScrollTop = 0;
	let history = $state<Array<{ request: MenuRequest; parentIndex: number }>>([]);
	const current = $derived(history.at(-1)?.request ?? request);
	const actions = $derived<MenuAction[]>(history.length
		? [{ label: 'Back', run: back }, ...inlineSingleActions(current.actions)]
		: inlineSingleActions(current.actions));

	function back() {
		const previous = history.at(-1);
		if (!previous) return;
		history = history.slice(0, -1);
		void focus(previous.parentIndex);
	}

	function openSubmenu(index: number) {
		clearTimeout(hover);
		if (closed) return;
		const action = actions[index];
		const invoker = menu.querySelectorAll<HTMLButtonElement>(':scope > button')[index];
		if (action?.disabled || !action?.children?.length || !invoker) return;
		active = index;
		const next = { label: action.label, actions: action.children, invoker, x: 0, y: 0 };
		if (sheet) {
			history = [...history, { request: next, parentIndex: index }];
			void focus(0);
		} else child = next;
	}

	function hoverItem(event: PointerEvent, index: number) {
		// A viewport pan can move a sibling under a stationary mouse.
		// Wait for a pointer move before treating that as hover intent.
		if (event.pointerType !== 'mouse' || hovered === event.currentTarget) return;
		hovered = event.currentTarget;
		clearTimeout(hover);
		if (child?.invoker !== hovered) dismissSubmenu();
		if (!actions[index].disabled && actions[index].children) hover = setTimeout(() => openSubmenu(index), 300);
	}

	function leaveItem(event: PointerEvent) {
		if (event.pointerType !== 'mouse' || hovered !== event.currentTarget) return;
		hovered = null;
		clearTimeout(hover);
	}

	function dismissSubmenu() {
		// Restore only focus owned by the removed flyout, not a newer choice
		// elsewhere in the root. Focus its surviving invoker before teardown.
		if (child && menu.contains(document.activeElement) && document.activeElement?.closest('[role="menu"]') !== menu) {
			child.invoker.focus({ preventScroll: true });
		}
		child = null;
	}

	function finish() {
		if (onactivate) onactivate();
		else close();
	}

	function activate(action: MenuAction, index: number) {
		if (action.disabled) return;
		if (history.length && index === 0) { back(); return; }
		if (action.children) openSubmenu(index);
		else { finish(); action.run(); }
	}

	function close(restore = true) {
		if (closed) return;
		closed = true;
		clearTimeout(hover);
		const invoker = opening.invoker;
		onclose();
		if (restore && invoker.isConnected) invoker.focus({ preventScroll: true });
	}

	function place() {
		const viewport = window.visualViewport;
		const width = viewport?.width ?? document.documentElement.clientWidth;
		const height = viewport?.height ?? document.documentElement.clientHeight;
		const offsetX = viewport?.offsetLeft ?? 0;
		const offsetY = viewport?.offsetTop ?? 0;
		menu.style.setProperty('--menu-viewport-width', `${width}px`);
		menu.style.setProperty('--menu-viewport-height', `${height}px`);
		if (sheet) {
			// The sheet rises from this edge as its contents change.
			menu.style.left = `${offsetX}px`;
			menu.style.top = `${offsetY + height}px`;
			return;
		}

		// Measure with the new size limits. Write the position synchronously
		// so each open flyout measures its parent at its updated position.
		const rect = menu.getBoundingClientRect();
		let point;
		if (submenu) {
			const anchor = opening.invoker.getBoundingClientRect();
			const right = anchor.right - offsetX;
			const x = right + rect.width > width ? anchor.left - offsetX - rect.width : right;
			point = {
				left: Math.max(0, Math.min(x, width - rect.width)),
				top: Math.max(0, Math.min(anchor.top - offsetY, height - rect.height))
			};
		} else {
			point = menuPosition(opening.x - offsetX, opening.y - offsetY, rect.width, rect.height, width, height);
		}
		menu.style.left = `${point.left + offsetX}px`;
		menu.style.top = `${point.top + offsetY}px`;
		// Growing the viewport can clamp the menu's scroll position.
		menuScrollTop = menu.scrollTop;
	}

	function reveal(item: HTMLButtonElement) {
		if (item.offsetTop < menu.scrollTop) menu.scrollTop = item.offsetTop;
		else if (item.offsetTop + item.offsetHeight > menu.scrollTop + menu.clientHeight) {
			menu.scrollTop = item.offsetTop + item.offsetHeight - menu.clientHeight;
		}
		menuScrollTop = menu.scrollTop;
	}

	async function focus(index: number) {
		clearTimeout(hover);
		child = null;
		active = index;
		await tick();
		if (closed) return;
		const item = menu.querySelectorAll<HTMLButtonElement>(':scope > button')[index];
		if (item) {
			item.focus({ preventScroll: true });
			reveal(item);
		}
	}

	function keydown(event: KeyboardEvent) {
		// Item type-ahead must never reach the player's letter shortcuts.
		event.stopPropagation();
		const count = actions.length;
		if ((event.key === 'Escape' || event.key === 'ArrowLeft') && history.length) { event.preventDefault(); back(); }
		else if (event.key === 'Escape' || (event.key === 'ArrowLeft' && submenu)) { event.preventDefault(); close(); }
		else if (event.key === 'ArrowRight' && actions[active]?.children) { event.preventDefault(); openSubmenu(active); }
		else if (event.key === 'Tab') { finish(); }
		else if (['ArrowDown', 'ArrowUp', 'Home', 'End'].includes(event.key)) {
			event.preventDefault();
			void focus(event.key === 'Home' ? 0 : event.key === 'End' ? count - 1 :
				(active + (event.key === 'ArrowDown' ? 1 : count - 1)) % count);
		} else if (event.key.length === 1 && !event.ctrlKey && !event.metaKey && !event.altKey && event.key !== ' ') {
			event.preventDefault();
			const now = Date.now();
			prefix = (now - typedAt > 500 ? '' : prefix) + event.key.toLocaleLowerCase();
			typedAt = now;
			const index = actions.findIndex((action) => !action.disabled && action.label.toLocaleLowerCase().startsWith(prefix));
			if (index >= 0) void focus(index);
		}
	}

	onMount(() => {
		// The parent can clear its request before teardown or the recovery tick.
		opening = request;
		const { invoker, fallbackFocus } = opening;
		// Keep the open tree's presentation through a resize so an open
		// flyout or a sheet's Back history keeps the same navigation model.
		sheet = !submenu && window.matchMedia('(max-width: 767px) and (pointer: coarse)').matches;
		menu.showPopover();
		place();
		ready = true;
		const initial = request.initialIndex;
		const firstEnabled = initial !== undefined && actions[initial] && !actions[initial].disabled
			? initial : actions.findIndex((action) => !action.disabled);
		void focus(Math.max(0, firstEnabled));
		const outside = (event: MouseEvent) => {
			// Sheet navigation can replace the clicked item during bubbling.
			if (!event.composedPath().includes(menu)) close();
		};
		const focusOutside = (event: FocusEvent) => {
			if (event.target instanceof Node && !menu.contains(event.target)) close(false);
		};
		const scrollX = window.scrollX;
		const scrollY = window.scrollY;
		const scroll = (event: Event) => {
			if (event.target instanceof Node && menu.contains(event.target)) {
				if (event.target === menu && menu.scrollTop !== menuScrollTop) {
					menuScrollTop = menu.scrollTop;
					dismissSubmenu();
				}
				return;
			}
			// A click can scroll its invoker before this listener mounts, with
			// delivery deferred until the next frame. That is not a new scroll.
			if (event.target === document && window.scrollX === scrollX && window.scrollY === scrollY) return;
			close();
		};
		// The root owns dismissal for the whole DOM-contained menu tree.
		// A child must not restore focus into a root that is being dismissed.
		// Let the opening click finish bubbling before listening. Bubble phase
		// also lets the row consume a long-press's synthesized click first.
		const resize = () => {
			place();
			const item = document.activeElement;
			if (item instanceof HTMLButtonElement && item.parentElement === menu) reveal(item);
		};
		const listeners = [listen(window, 'resize', resize)];
		const openingClick = setTimeout(() => {
			if (!submenu && !closed) listeners.push(listen(document, 'click', outside));
		}, 0);
		if (!submenu) {
			listeners.push(listen(document, 'focusin', focusOutside), listen(window, 'scroll', scroll, true));
		}
		if (window.visualViewport) {
			listeners.push(listen(window.visualViewport, 'resize', resize), listen(window.visualViewport, 'scroll', place));
		}
		return () => {
			// Snapshot invalidation can unmount the menu without an activation.
			// Recover only focus that belonged to it, never a newer user choice.
			if (!submenu && !closed && (menu.contains(document.activeElement) || document.activeElement === document.body)) {
				void tick().then(() => {
					if (document.activeElement !== document.body && document.activeElement?.isConnected) return;
					if (invoker.isConnected) invoker.focus({ preventScroll: true });
					else fallbackFocus?.();
				});
			}
			clearTimeout(hover);
			clearTimeout(openingClick);
			stopListening(listeners);
		};
	});
</script>

<div
	bind:this={menu}
	popover="manual"
	role="menu"
	aria-label={current.label}
	tabindex="-1"
	data-testid="track-context-menu"
	class:sheet
	style:visibility={ready ? 'visible' : 'hidden'}
	onkeydown={keydown}
>
	{#each actions as action, index}
		<button
			type="button"
			role={action.checked === undefined ? 'menuitem' : 'menuitemcheckbox'}
			aria-checked={action.checked}
			aria-disabled={action.disabled || undefined}
			aria-label={action.hint ? `${action.label}, ${action.hint.toLocaleLowerCase()}` : undefined}
			aria-haspopup={action.children ? 'menu' : undefined}
			aria-expanded={action.children ? child?.label === action.label : undefined}
			tabindex={active === index && !child ? 0 : -1}
			class:destructive={action.destructive}
			onfocus={() => (active = index)}
			onpointermove={(event) => hoverItem(event, index)}
			onpointerleave={leaveItem}
			onclick={() => activate(action, index)}
		>
			{action.label}{#if action.checked}<span aria-hidden="true"> ✓</span>{/if}{#if action.children}<span aria-hidden="true"> ▸</span>{/if}
			{#if action.hint}<span class="hint">{action.hint}</span>{/if}
		</button>
	{/each}
	{#if child}
		{#key child}
			<ContextMenu request={child} submenu onclose={() => (child = null)} onactivate={finish} />
		{/key}
	{/if}
</div>

<style>
	[role='menu'] {
		position: fixed;
		inset: auto;
		margin: 0;
		padding: var(--nt-space-1);
		width: max-content;
		min-width: min(200px, var(--menu-viewport-width, 100vw));
		max-width: var(--menu-viewport-width, 100vw);
		max-height: min(80dvh, calc(var(--menu-viewport-height, 100dvh) * .8));
		overflow-y: auto;
		box-sizing: border-box;
		border: 1px solid var(--nt-border);
		border-radius: var(--nt-radius);
		background: var(--nt-bg-raised);
		color: var(--nt-fg);
	}
	button {
		display: block;
		width: 100%;
		min-height: 40px;
		padding: var(--nt-space-2) var(--nt-space-3);
		border: 0;
		background: transparent;
		color: inherit;
		font: inherit;
		text-align: start;
		overflow-wrap: anywhere;
		cursor: pointer;
	}
	button:not([aria-disabled='true']):hover, button:not([aria-disabled='true']):focus-visible { background: var(--nt-border); }
	button:focus-visible { outline: 2px solid var(--nt-accent); outline-offset: -2px; }
	.destructive { color: var(--nt-bad); }
	button[aria-disabled='true'], .hint { color: var(--nt-fg-muted); }
	button[aria-disabled='true'] { cursor: default; }
	.hint { display: block; }
	.sheet {
		width: var(--menu-viewport-width, 100vw);
		transform: translateY(-100%);
		border-radius: var(--nt-radius) var(--nt-radius) 0 0;
	}
	@media (pointer: coarse) { button { min-height: 48px; } }
</style>
