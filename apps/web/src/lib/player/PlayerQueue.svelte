<script lang="ts">
	import { tick } from 'svelte';
	import { currentTrack, type QueueState, type Track } from './queue.js';
	import { matchesQueueQuery } from './queueFilter.js';
	import ShuffleUpcoming from './ShuffleUpcoming.svelte';
	import type { QueueSelection, QueueSelectionAction } from './queueSelection.js';
	import type { PlayerSession } from './session.js';
	import ContextMenu from '../interactions/ContextMenu.svelte';
	import MoreActionsGlyph from '../interactions/MoreActionsGlyph.svelte';
	import { pointerQueueDrag } from '../interactions/pointer-queue-drag.js';
	import { contextMenu, menuAt, unavailablePlaylistAction, type MenuRequest, type MenuAction } from '../interactions/context-menu.js';
	import { activeTrackDrag, droppedTrackDrag, startTrackDrag, endTrackDrag } from '../interactions/track-drag.js';

	type SelectionControls = Pick<PlayerSession, 'toggleQueueSelection' | 'selectAllQueueRows' | 'clearQueueSelection' | 'editQueueSelection'>;

	interface Props {
		state: QueueState;
		/** Opt-in, session-owned multi-selection; demo and isolated panels omit it. */
		selection?: QueueSelection;
		selectionControls?: SelectionControls;
		/** Transport stays with the host page: called when Next is activated. */
		onnext?: () => void;
		/** Queue-only edit: the host must not restart playback. */
		onshuffle?: () => void;
		radioActive?: boolean;
		/** Queue management stays with the host page too: the panel reports intent, never mutates. */
		onmove?: (from: number, to: number) => void;
		/** Stable-id drag intents: host resolves against its current, authorized catalogue. */
		oninsert?: (ids: readonly string[], beforeId: string | null) => void;
		interactionsUnavailable?: boolean;
		/** Host recovery target when invalidation unmounts the whole queue. */
		onmenufocusfallback?: () => void;
		onreorder?: (ids: readonly string[], beforeId: string | null) => void;
		onplaynextids?: (ids: readonly string[]) => void;
		onremoveids?: (ids: readonly string[]) => void;
		/** Explicit transport gesture, distinct from metadata-only Select. */
		onplaynow?: (id: string) => void;
		onplaynext?: (index: number) => void;
		onremove?: (index: number) => void;
		onclear?: () => void;
		/** Favorites stay with the library host; other queues omit the intent. */
		favoriteIds?: ReadonlySet<string>;
		ontogglefavorite?: (id: string) => void;
		/** Aggregate favorite mutation by stable ids (menus); preferred over ontogglefavorite when supplied. */
		onfavoriteids?: (ids: readonly string[]) => void;
		/**
		 * Metadata-only direct selection: the host applies the validated
		 * selectTrack transition and owns the announcement; the panel reports
		 * the ORIGINAL queue index once and never mutates, transports or
		 * fetches. Each Select control renders only when provided.
		 */
		onselect?: (index: number) => void;
		/**
		 * Share intent by stable track id: the host builds the link, owns the
		 * clipboard and the polite outcome. The panel only reports and never
		 * mutates, selects or plays. The control renders only when provided.
		 */
		oncopylink?: (id: string) => void;
		/**
		 * Trim intents, same contract: the host applies the transition and
		 * owns the announcement; the panel reports once and never mutates.
		 * Each control renders only when its callback is provided.
		 */
		onkeepcurrentandupcoming?: () => void;
		onclearupcoming?: () => void;
		/** Polite queue-action announcements, owned by the host; the panel only renders the region. */
		announcement?: string;
		/** Real action identity: equal outcome text must still update the live region. */
		announcementVersion?: number;
		/**
		 * Transient queue-only filter text. Bindable so a host that
		 * unmounts/remounts the panel (library refresh swaps branches)
		 * can keep the query across mounts; unbound parents get the same
		 * component-local default. Never persisted or fetched.
		 */
		filterQuery?: string;
		/**
		 * Whether the panel's polite regions are exposed as live statuses.
		 * The library host suspends them while a library refresh is in
		 * flight so the loading transient keeps a single page-level
		 * status; content, controls and focus are untouched. The open consent
		 * warning stays live so a changed destructive scope is never silent.
		 * Defaults to exposed; component harnesses leave it on.
		 */
		liveRegions?: boolean;
		/** The persistent player owns identity and Next while its transport is mounted. */
		showTransport?: boolean;
		/** A persistent host may render the action region outside this panel. */
		showAnnouncement?: boolean;
		/**
		 * Opt-in two-step guard for the two destructive intents (Clear queue,
		 * Clear upcoming): the panel opens an inline Cancel/Confirm prompt and
		 * reports the intent only after Confirm. Hosts that omit it keep the
		 * one-step behavior.
		 */
		confirmDestructive?: boolean;
		/** Host increments when implicit reconciliation replaces the consented arrangement. */
		confirmGeneration?: number;
	}

	// Renamed locally: a `state` binding would shadow the `$state` rune.
	let {
		state: queue,
		selection,
		selectionControls,
		onnext,
		onshuffle,
		radioActive = false,
		onmove,
		oninsert,
		interactionsUnavailable = false,
		onmenufocusfallback,
		onreorder,
		onplaynextids,
		onremoveids,
		onplaynow,
		onplaynext,
		onremove,
		onclear,
		onselect,
		oncopylink,
		favoriteIds = new Set<string>(),
		ontogglefavorite,
		onfavoriteids,
		onkeepcurrentandupcoming,
		onclearupcoming,
		announcement = '',
		announcementVersion = 0,
		filterQuery = $bindable(''),
		liveRegions = true,
		showTransport = true,
		showAnnouncement = true,
		confirmDestructive = false,
		confirmGeneration = 0
	}: Props = $props();

	const confirmId = $props.id();

	let current = $derived(currentTrack(queue));

	/**
	 * Defensive selected slot: only an in-range integer index counts as a
	 * selection. Anything else (empty queue, negative, past-end, fractional,
	 * NaN, infinite) renders the unselected summary and no current row —
	 * never a clamped or fabricated position.
	 */
	let selectedIndex = $derived(
		Number.isInteger(queue.currentIndex) &&
			queue.currentIndex >= 0 &&
			queue.currentIndex < queue.tracks.length
			? queue.currentIndex
			: -1
	);

	let trackCount = $derived(queue.tracks.length);
	let upcoming = $derived(selectedIndex < 0 ? 0 : trackCount - selectedIndex - 1);

	const dragVersion = $derived({ queue, confirmGeneration, interactionsUnavailable });
	let menuHeading: HTMLHeadingElement | null = $state(null);
	let list: HTMLOListElement | null = $state(null);
	let emptyTarget: HTMLElement | null = $state(null);
	let filterEmptyTarget: HTMLElement | null = $state(null);
	let revealCurrentButtonEl: HTMLButtonElement | null = $state(null);

	function currentRowElement(): HTMLElement | null {
		return (
			list?.querySelector<HTMLElement>('[data-testid="queue-item"][aria-current="true"]') ?? null
		);
	}

	/**
	 * Stable-id survivor for remove recovery: the first id after the removed
	 * one in the pre-action displayed order that still exists, otherwise the
	 * nearest preceding one. Re-resolved against the live DOM after render,
	 * never from stale array indices.
	 */
	function survivorId(
		beforeIds: string[],
		removedId: string | undefined,
		current: Set<string>
	): string | null {
		const at = removedId === undefined ? -1 : beforeIds.indexOf(removedId);
		return beforeIds.slice(at + 1).find((id) => current.has(id)) ??
			beforeIds.slice(0, Math.max(0, at)).reverse().find((id) => current.has(id)) ?? null;
	}

	function focusRemoveFor(trackId: string): boolean {
		return focusRowControl(trackId, 'button[aria-label^="Remove "]');
	}

	function focusRowControl(trackId: string, selector: string): boolean {
		const rows = list?.querySelectorAll('[data-testid="queue-item"]') ?? [];
		for (const row of rows) {
			if (row.getAttribute('data-track-id') !== trackId) continue;
			const remove = row.querySelector<HTMLButtonElement>(selector);
			if (!remove) return false;
			focusVisible(remove);
			return true;
		}
		return false;
	}

	function focusEmptyTarget(): void {
		focusVisible(emptyTarget);
	}

	/**
	 * Zero-visible fallback: tracks survive behind the filter but no row
	 * renders, so the no-matches status — not the "Queue is empty" target,
	 * which would misannounce — receives recovery focus.
	 */
	function focusFilterEmptyTarget(): boolean {
		if (!filterEmptyTarget) return false;
		focusVisible(filterEmptyTarget);
		return true;
	}

	/**
	 * Focus is only recovered when this explicit action stranded it: the
	 * activated control held focus at dispatch and, after render, focus sits
	 * on nothing (body/null) — or is still on that same control while the
	 * action just disabled it. DOM removal strands synchronously, but a
	 * control that becomes disabled keeps focus through the flush microtask
	 * and the browser drops it to the body on the next rendering steps, so
	 * waiting for body/null alone would miss Clear every time. Any focus
	 * that has moved on to a different live element — Tab movement, pointer
	 * flow, host-driven restore — is left alone, and prop-only updates
	 * (refresh, auth purge) never reach here.
	 */
	function focusStranded(heldFocus: boolean, button: EventTarget | null): boolean {
		if (!heldFocus) return false;
		const active = document.activeElement;
		if (active === null || active === document.body) return true;
		return (
			button instanceof HTMLButtonElement && button.disabled && active === button
		);
	}

	/**
	 * Report one remove intent, then land focus deterministically: the next
	 * surviving displayed row's Remove, otherwise the previous, otherwise
	 * the no-matches status when hidden tracks survive behind the filter,
	 * otherwise the stable empty target. Consecutive keyboard removals stay
	 * operable because each recovery lands on a live Remove control.
	 */
	async function reportRemove(event: Event, index: number): Promise<void> {
		// currentTarget and the pre-action order are only valid during dispatch.
		const button = event.currentTarget;
		const heldFocus = button instanceof HTMLElement && document.activeElement === button;
		const removedId = queue.tracks[index]?.id;
		const beforeIds = visibleEntries.map((entry) => entry.track.id);
		onremove?.(index);
		await tick();
		if (!focusStranded(heldFocus, button)) return;
		const current = new Set<string>();
		for (const row of list?.querySelectorAll('[data-testid="queue-item"]') ?? []) {
			const id = row.getAttribute('data-track-id');
			if (id !== null) current.add(id);
		}
		const target = survivorId(beforeIds, removedId, current);
		if (target !== null && focusRemoveFor(target)) return;
		// No surviving displayed row. When tracks survive but the filter
		// hides every one of them, the honest no-matches status takes
		// recovery focus; only a truly empty queue uses the empty target.
		if (queue.tracks.length > 0 && focusFilterEmptyTarget()) return;
		focusEmptyTarget();
	}

	/**
	 * Report the clear intent, then land focus on the stable empty target
	 * instead of the now-disabled Clear button or the body.
	 */
	async function reportClear(event: Event): Promise<void> {
		const button = event.currentTarget;
		const heldFocus = button instanceof HTMLElement && document.activeElement === button;
		onclear?.();
		await tick();
		if (!focusStranded(heldFocus, button)) return;
		focusEmptyTarget();
	}

	/**
	 * Pending destructive confirm. Panel-local on purpose: it dies with the
	 * panel (route unmount, access-refusal alert branch) and dismisses itself
	 * the moment its precondition goes away (queue emptied by an auth
	 * transition, nothing left upcoming), so it can never act on, or survive
	 * into, another account's queue.
	 */
	let pending: 'clear' | 'trim' | null = $state(null);
	let pendingTrigger: HTMLButtonElement | null = null;
	let pendingAction: (() => void) | undefined = $state();
	let pendingGeneration = $state(0);
	let confirmEl: HTMLFieldSetElement | null = $state(null);
	let cancelButtonEl: HTMLButtonElement | null = $state(null);
	let cancelNote = $state('');
	let cancelNoteVersion = $state(0);

	let pendingValid = $derived(
		confirmDestructive && pendingGeneration === confirmGeneration &&
		(pending === 'clear'
			? trackCount > 0 && onclear !== undefined && pendingAction === onclear
			: pending === 'trim' && upcoming > 0 && onclearupcoming !== undefined && pendingAction === onclearupcoming)
	);

	/** A disabled control may retain DOM focus until after the render flush. */
	function hasLiveFocus(): boolean {
		const active = document.activeElement;
		return active instanceof HTMLElement && active !== document.body &&
			active.isConnected && !active.matches(':disabled') && active.getAttribute('aria-disabled') !== 'true';
	}

	function focusVisible(target: HTMLElement | null): void {
		target?.focus({ preventScroll: true });
		target?.scrollIntoView({ block: 'nearest', behavior: 'instant' });
	}

	function recoverConfirmFocus(): void {
		if (hasLiveFocus()) return;
		if (trackCount === 0) focusEmptyTarget();
		else focusVisible(currentRowElement() ?? revealCurrentButtonEl);
	}

	async function dismissConfirm(): Promise<void> {
		// Capture ownership BEFORE invalidity removes the focused prompt. An
		// unrelated, still-live focus choice must survive background updates.
		const heldFocus = confirmEl?.contains(document.activeElement) ?? false;
		pending = null;
		pendingTrigger = null;
		pendingAction = undefined;
		await tick();
		if (heldFocus) recoverConfirmFocus();
	}

	$effect.pre(() => {
		if (pending !== null && !pendingValid) void dismissConfirm();
	});

	async function openConfirm(kind: 'clear' | 'trim', event: Event): Promise<void> {
		const button = event.currentTarget;
		pendingTrigger = button instanceof HTMLButtonElement ? button : null;
		cancelNote = '';
		pendingAction = kind === 'clear' ? onclear : onclearupcoming;
		pendingGeneration = confirmGeneration;
		pending = kind;
		await tick();
		// Safe default first; Tab reaches Confirm next. Never hide keyboard focus.
		if (pending === kind && pendingValid) focusVisible(cancelButtonEl);
	}

	function requestClear(event: Event): void {
		if (!confirmDestructive) {
			void reportClear(event);
			return;
		}
		if (trackCount === 0) return;
		void openConfirm('clear', event);
	}

	function requestTrim(event: Event): void {
		if (!confirmDestructive) {
			void reportTrim(event, onclearupcoming);
			return;
		}
		if (upcoming <= 0) return;
		void openConfirm('trim', event);
	}

	/** Cancel is a pure UI reset: nothing is reported, focus returns to the trigger. */
	async function cancelConfirm(): Promise<void> {
		const trigger = pendingTrigger;
		const kind = pending;
		if (kind === null) return;
		pending = null;
		pendingTrigger = null;
		pendingAction = undefined;
		cancelNote = kind === 'trim' ? 'Clear upcoming cancelled. Queue kept.' : 'Clear queue cancelled. Queue kept.';
		cancelNoteVersion += 1;
		await tick();
		focusVisible(trigger);
	}

	async function acceptConfirm(event: Event): Promise<void> {
		// A detached control from a dismissed prompt never authorizes a later one.
		if (!(event.currentTarget instanceof HTMLButtonElement) || !event.currentTarget.isConnected) return;
		if (pending === null || !pendingValid) return;
		const action = pendingAction;
		pending = null;
		pendingTrigger = null;
		pendingAction = undefined;
		cancelNote = '';
		action?.();
		await tick();
		// Recover any control disabled/removed by this action, but never steal
		// a newer, still-live focus choice. Filtering stays explicit and intact.
		recoverConfirmFocus();
	}

	let normalizedQuery = $derived(filterQuery.trim().toLowerCase());

	/** Blank titles render (and match) as their stable track id, never empty. */
	function displayTitle(track: Track): string {
		return track.title?.trim() ? track.title : track.id;
	}

	/**
	 * Shared presentation matcher: `normalized` is already trimmed and
	 * lowercased, and the shared export re-normalizes idempotently, so
	 * this delegation renders byte-identical output to the inline version.
	 */
	function matchesQuery(track: Track, normalized: string): boolean {
		return matchesQueueQuery(track, normalized);
	}

	/**
	 * Visible rows with their ORIGINAL queue indices: every row action,
	 * disabled boundary and the current marker use the full-queue slot, never
	 * the filtered position. Pure reads only — frozen props stay untouched.
	 */
	let visibleEntries = $derived(
		queue.tracks
			.map((track, index) => ({ track, index }))
			.filter(({ track }) => matchesQuery(track, normalizedQuery))
	);
	/**
	 * Explicit reveal only: bound to the Show-current-track button's click.
	 * Clears a hiding filter first, then focuses/reveals the real row.
	 * Never runs from an effect, never mutates state, never calls transport.
	 * Focus uses preventScroll and the scroll is instant (`block: nearest`,
	 * never smooth), so no reduced-motion special case is needed.
	 */
	async function revealCurrent(): Promise<void> {
		if (selectedIndex < 0) return;
		if (!visibleEntries.some((entry) => entry.index === selectedIndex)) {
			filterQuery = '';
			await tick();
		}
		const row = currentRowElement();
		if (!row) return;
		focusVisible(row);
	}

	/**
	 * Report one trim intent, then land focus deterministically: a trim that
	 * removed the rows keeping its control enabled leaves a disabled button
	 * behind, so focus moves to the surviving current row instead of the
	 * body. Runs only on explicit activation — prop-only updates never reach
	 * here, so background refreshes cannot steal focus.
	 */
	async function reportTrim(event: Event, notify: (() => void) | undefined): Promise<void> {
		// currentTarget is only valid during dispatch; capture it before the tick.
		const button = event.currentTarget;
		notify?.();
		await tick();
		if (button instanceof HTMLButtonElement && button.disabled) {
			currentRowElement()?.focus({ preventScroll: true });
		}
	}

	/** Native controls keep their own click/Space behavior; row gestures never invoke them. */
	function selectionClick(event: MouseEvent): void {
		if (!selectionControls || event.defaultPrevented || event.altKey ||
			!(event.ctrlKey || event.metaKey || event.shiftKey) || !(event.target instanceof Element)) return;
		if (event.target.closest('button, input, select, textarea, a, label, [contenteditable]')) return;
		const row = event.target.closest<HTMLElement>('[data-testid="queue-item"]');
		const id = row?.dataset.trackId;
		if (!row || !id || !list?.contains(row)) return;
		event.preventDefault();
		selectionControls.toggleQueueSelection(id, event.shiftKey);
	}

	function selectionKey(event: KeyboardEvent): void {
		if (!selectionControls || event.defaultPrevented || event.repeat || event.isComposing ||
			event.altKey || !(event.target instanceof HTMLElement) ||
			event.target.closest('textarea, select, [contenteditable], input:not([type="checkbox"])')) return;
		if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === 'a') {
			event.preventDefault();
			selectionControls.selectAllQueueRows();
		} else if (event.key === ' ' && !event.ctrlKey && !event.metaKey && !event.shiftKey &&
			selection?.active && event.target.matches('[data-testid="queue-item"]')) {
			event.preventDefault();
			selectionControls.toggleQueueSelection(event.target.dataset.trackId!);
		}
	}

	async function reportSelection(event: Event, action?: QueueSelectionAction): Promise<void> {
		const button = event.currentTarget;
		const heldFocus = button instanceof HTMLElement && document.activeElement === button;
		const beforeIds = visibleEntries.map((entry) => entry.track.id);
		const first = beforeIds.find((id) => selection?.ids.includes(id));
		const generation = confirmGeneration;
		if (!selectionControls) return;
		if (action) {
			if (!selectionControls.editQueueSelection(action)) return;
		} else selectionControls.clearQueueSelection();
		await tick();
		if (generation !== confirmGeneration || !focusStranded(heldFocus, button)) return;
		const rows = [...(list?.querySelectorAll<HTMLElement>('[data-testid="queue-item"]') ?? [])];
		const ids = new Set(rows.map((row) => row.dataset.trackId!));
		const target = action === 'remove' ? survivorId(beforeIds, first, ids) : first ?? beforeIds[0];
		const row = rows.find((row) => row.dataset.trackId === target);
		const checkbox = row?.querySelector<HTMLElement>('input[type="checkbox"]');
		if (checkbox) focusVisible(checkbox);
		else if (queue.tracks.length > 0 && focusFilterEmptyTarget()) return;
		else focusEmptyTarget();
	}

	/**
	 * Report one direct-selection intent with the ORIGINAL queue coordinate.
	 * Runs only on explicit activation — prop-only updates never reach here,
	 * so background refreshes cannot steal focus. A detached button (removed
	 * row) or an identity mismatch (stale closure over a reused slot) reports
	 * nothing, so a removed identity can never select its replacement. The
	 * host validates again and applies the selectTrack transition.
	 */
	let trackMenu = $state<MenuRequest | null>(null);
	let dropRow = $state<string | null>(null);
	let dropAfter = $state(false);
	const batchIds = $derived(selection?.ids ?? []);
	const orderedBatch = $derived(queue.tracks.filter((track) => batchIds.includes(track.id)).map((track) => track.id));
	let moveBefore = $state('');
	$effect(() => {
		if (moveBefore && (batchIds.includes(moveBefore) || !queue.tracks.some((track) => track.id === moveBefore))) {
			moveBefore = '';
		}
	});

	function batchFor(id: string): string[] {
		return batchIds.includes(id) ? orderedBatch : [id];
	}

	$effect(() => {
		void queue;
		void confirmGeneration;
		void interactionsUnavailable;
		void filterQuery;
		trackMenu = null;
		dropRow = null;
	});

	function toggleFavorites(ids: readonly string[]): void {
		const live = ids.filter((id) => queue.tracks.some((track) => track.id === id));
		if (live.length === 0) return;
		if (onfavoriteids) onfavoriteids(live);
		else {
			const remove = live.every((id) => favoriteIds.has(id));
			for (const id of live.filter((id) => favoriteIds.has(id) === remove)) ontogglefavorite?.(id);
		}
	}

	/** Next surviving More button, otherwise previous, otherwise the heading. */
	function focusMenuSurvivor(beforeIds: string[], removedId: string | undefined): void {
		const survivor = survivorId(beforeIds, removedId, new Set(visibleEntries.map(({ track }) => track.id)));
		if (survivor && focusRowControl(survivor, '[data-testid="queue-more"]')) return;
		if (menuHeading?.isConnected) menuHeading.focus({ preventScroll: true });
		else onmenufocusfallback?.();
	}

	function recoverMenuRemoval(beforeIds: string[], removedId: string | undefined): void {
		void tick().then(() => {
			if (document.activeElement !== document.body && document.activeElement?.isConnected) return;
			focusMenuSurvivor(beforeIds, removedId);
		});
	}

	function openTrackMenu(id: string, invoker: HTMLElement, point?: { x: number; y: number }, focusMove = false): void {
		if (interactionsUnavailable || (!oninsert && !onreorder)) return;
		const index = queue.tracks.findIndex((track) => track.id === id);
		if (index < 0) return;
		const at = () => queue.tracks.findIndex((track) => track.id === id);
		const move = (to: number) => { const from = at(); if (from >= 0) onmove?.(from, to); };
		const actions: MenuAction[] = [];
		const ids = batchFor(id);
		const target = invoker.matches('li') ? invoker.querySelector<HTMLElement>('[data-testid="queue-more"]') ?? invoker : invoker;
		let request: MenuRequest;
		if (ids.length > 1 && onreorder) {
			const all = queue.tracks.map((track) => track.id);
			const atTop = all.slice(0, ids.length).every((x) => ids.includes(x));
			const atBottom = all.slice(all.length - ids.length).every((x) => ids.includes(x));
			if (onplaynextids) actions.push({ label: 'Play next', run: () => onplaynextids?.(ids) });
			if (!atTop) actions.push({ label: 'Move to top', run: () => onreorder?.(ids, queue.tracks.find((track) => !ids.includes(track.id))?.id ?? null) });
			if (!atBottom) actions.push({ label: 'Move to bottom', run: () => onreorder?.(ids, null) });
			if (onfavoriteids || ontogglefavorite) {
				const starred = ids.every((x) => favoriteIds.has(x));
				actions.push({ label: starred ? 'Unstar all' : 'Star all', run: () => toggleFavorites(ids) });
			}
			actions.push(unavailablePlaylistAction);
			if (onremoveids) actions.push({ label: 'Remove from queue', destructive: true, run: () => {
				const beforeIds = visibleEntries.map(({ track }) => track.id);
				onremoveids?.(ids);
				recoverMenuRemoval(beforeIds, ids[0]);
			} });
			request = { ...menuAt(target, `${ids.length} selected tracks`, actions), ...point };
		} else {
			const last = queue.tracks.length - 1;
			if (onplaynow) actions.push({ label: 'Play now', run: () => { if (at() >= 0) onplaynow?.(id); } });
			if (onplaynext) actions.push({ label: 'Play next', run: () => { const i = at(); if (i >= 0) onplaynext?.(i); } });
			if (onmove && index > 0) {
				actions.push({ label: 'Move to top', run: () => move(0) });
				actions.push({ label: 'Move up', run: () => move(at() - 1) });
			}
			if (onmove && index < last) {
				actions.push({ label: 'Move down', run: () => move(at() + 1) });
				actions.push({ label: 'Move to bottom', run: () => move(queue.tracks.length - 1) });
			}
			if (onfavoriteids || ontogglefavorite) {
				const starred = favoriteIds.has(id);
				actions.push({ label: starred ? 'Unstar' : 'Star', run: () => toggleFavorites([id]) });
			}
			actions.push(unavailablePlaylistAction);
			if (oncopylink) actions.push({ label: 'Copy link', run: () => { if (at() >= 0) oncopylink?.(id); } });
			if (onremove) actions.push({ label: 'Remove from queue', destructive: true, run: () => {
				const i = at();
				if (i < 0) return;
				const beforeIds = visibleEntries.map((entry) => entry.track.id);
				onremove?.(i);
				recoverMenuRemoval(beforeIds, id);
			} });
			request = { ...menuAt(target, `Actions for ${displayTitle(queue.tracks[index]!)}`, actions), ...point };
		}
		if (focusMove) request.initialIndex = actions.findIndex((action) => action.label.startsWith('Move'));
		const beforeIds = visibleEntries.map(({ track }) => track.id);
		request.fallbackFocus = () => focusMenuSurvivor(beforeIds, id);
		trackMenu = request;
	}

	// Touch/pen drag release on the grip can still synthesize a click; it must not open the menu.
	let gripDragged = false;
	let gripOrigin = { x: 0, y: 0 };
	function gripDown(event: PointerEvent): void {
		gripDragged = false;
		gripOrigin = { x: event.clientX, y: event.clientY };
	}
	function gripMove(event: PointerEvent): void {
		if (event.pointerType !== 'mouse' && Math.hypot(event.clientX - gripOrigin.x, event.clientY - gripOrigin.y) >= 6) gripDragged = true;
	}
	function gripClick(event: MouseEvent, id: string): void {
		event.stopPropagation();
		if (gripDragged && event.detail !== 0) { gripDragged = false; event.preventDefault(); return; }
		gripDragged = false;
		openTrackMenu(id, event.currentTarget as HTMLElement, undefined, true);
	}

	function dropDestination(event: Pick<DragEvent, 'target' | 'clientY'>, moving: readonly string[] = []): string | null {
		const row = event.target instanceof Element ? event.target.closest<HTMLElement>('[data-testid="queue-item"]') : null;
		const id = row?.dataset.trackId;
		const index = queue.tracks.findIndex((track) => track.id === id);
		if (!row || index < 0) { dropRow = null; return null; }
		dropRow = id ?? null;
		dropAfter = event.clientY > row.getBoundingClientRect().top + row.getBoundingClientRect().height / 2;
		// Resolve the boundary in the remaining queue, not inside the moving
		// group. Hidden rows still participate in the full-queue coordinates.
		return queue.tracks.slice(index + (dropAfter ? 1 : 0))
			.find((track) => !moving.includes(track.id))?.id ?? null;
	}

	function acceptsDrop(event: DragEvent): boolean {
		const drag = activeTrackDrag(event);
		return !interactionsUnavailable && !!drag && (drag.queue === list ? !!onreorder : drag.queue === null && !!oninsert);
	}

	function dragover(event: DragEvent): void {
		if (!acceptsDrop(event)) return;
		event.preventDefault();
		if (event.dataTransfer) event.dataTransfer.dropEffect = activeTrackDrag(event)?.queue ? 'move' : 'copy';
		dropDestination(event);
	}

	function drop(event: DragEvent): void {
		if (!acceptsDrop(event)) return;
		const drag = droppedTrackDrag(event);
		if (!drag) return;
		event.preventDefault();
		const beforeId = dropDestination(event, drag.ids);
		if (drag.queue === list) onreorder?.(drag.ids, beforeId);
		else oninsert?.(drag.ids, beforeId);
		dropRow = null;
		endTrackDrag();
	}

	function reportSelect(event: Event, index: number, trackId: string): void {
		const button = event.currentTarget;
		if (!(button instanceof HTMLButtonElement)) return;
		if (!button.isConnected) return;
		if (!onselect) return;
		if (queue.tracks[index]?.id !== trackId) return;
		if (!Number.isInteger(index) || index < 0 || index >= queue.tracks.length) return;
		onselect(index);
	}
</script>

<!--
	Reusable queue panel: renders a `QueueState` from the queue store with
	the same hooks the demo player and the queue-order specs rely on.
	The host page owns selection (via `onselect`), transport (via `onnext`)
	and management (via `onmove`/`onplaynext`/`onremove`/`onclear`); the
	panel itself never mutates state.
-->
{#if oninsert || onreorder}
	<h2 tabindex="-1" bind:this={menuHeading}>Queue</h2>
{/if}
{#if showTransport}
	<p data-testid="now-playing" role={liveRegions ? 'status' : undefined}>
		{#if current}
			<!-- String expression preserves the spaces around the separator. -->
			Now playing: {displayTitle(current)}{#if current.artist}{' — '}{current.artist}{/if}
		{:else}
			Queue is empty
		{/if}
	</p>
{/if}

<p data-testid="queue-position">
	{#if trackCount === 0}
		Queue selection: Queue is empty
	{:else if selectedIndex < 0}
		Queue selection: No track selected, {trackCount}
		{trackCount === 1 ? 'track' : 'tracks'}
	{:else}
		Queue selection: Track {selectedIndex + 1} of {trackCount}, {upcoming} upcoming
	{/if}
</p>

<!-- Keep the live region mounted; refresh only its content for equal-title actions. -->
{#if showAnnouncement}
	<p data-testid="queue-announcement" role={liveRegions ? 'status' : undefined}>{#key announcementVersion}{announcement}{/key}</p>
{/if}

{#if showTransport}
	<button
		type="button"
		data-testid="player-next"
		disabled={!current}
		onclick={() => onnext?.()}
	>
		Next
	</button>
{/if}

<button
	type="button"
	data-testid="queue-reveal-current"
	bind:this={revealCurrentButtonEl}
	disabled={selectedIndex < 0}
	onclick={revealCurrent}
>
	Show current track
</button>

<ShuffleUpcoming state={queue} {onshuffle} {radioActive} {liveRegions} />

<!--
	Queue-only filter: transient presentation state. It hides rows but
	never reorders, reselects, persists or fetches anything; the
	orientation summary above always describes the full queue.
-->
<div class="queue-filter">
	<label for="queue-filter-input">Filter queue</label>
	<input id="queue-filter-input" type="search" bind:value={filterQuery} />
	<button type="button" onclick={() => (filterQuery = '')}>Clear queue filter</button>
</div>

<p data-testid="queue-filter-count" role={liveRegions ? 'status' : undefined}>
	{visibleEntries.length} of {trackCount} tracks
</p>

{#if trackCount > 0 && visibleEntries.length === 0}
	<!--
		Zero-visible recovery target: programmatically focusable so a removal
		that leaves only hidden tracks behind lands here instead of the body.
		Focused only by explicit remove recovery, never on render; never a
		tab stop.
	-->
	<p data-testid="queue-filter-empty" tabindex="-1" bind:this={filterEmptyTarget}>No matches in this queue</p>
{/if}

{#if selection && selectionControls && selection.active}
	<div class="queue-filter" role="group" data-testid="queue-selection-toolbar" aria-label="Selected queue tracks">
		<p role={liveRegions ? 'status' : undefined}>{selection.ids.length} selected in queue</p>
		<button type="button" disabled={visibleEntries.length === 0} onclick={() => selectionControls?.selectAllQueueRows()}>Select all displayed</button>
		<button type="button" disabled={selection.ids.length === 0 || selectedIndex < 0} onclick={(event) => void reportSelection(event, 'playNext')}>Play next</button>
		{#if onreorder}
			<label>Move selected before
				<select bind:value={moveBefore}>
					<option value="">End of queue</option>
					{#each queue.tracks.filter((track) => !batchIds.includes(track.id)) as track (track.id)}
						<option value={track.id}>{displayTitle(track)}</option>
					{/each}
				</select>
			</label>
			<button type="button" disabled={selection.ids.length === 0} onclick={() => onreorder?.(orderedBatch, moveBefore || null)}>Move selected</button>
		{/if}
		<button type="button" disabled={selection.ids.length === 0} onclick={(event) => void reportSelection(event, 'remove')}>Remove selected from queue</button>
		<button type="button" onclick={(event) => void reportSelection(event)}>Done</button>
	</div>
	{@const hiddenCount = selection.ids.filter((id) => !visibleEntries.some(({ track }) => track.id === id)).length}
	{#if hiddenCount > 0}
		<p>{hiddenCount} selected {hiddenCount === 1 ? 'track is' : 'tracks are'} hidden by this filter.</p>
	{/if}
{/if}

<!-- svelte-ignore a11y_no_noninteractive_element_interactions: delegated row modifiers have native checkbox equivalents; controls keep their own keyboard/click behavior. -->
<ol aria-label="Playback queue" bind:this={list}
	data-testid="queue-drop-target"
	data-accepts-tracks={oninsert ? '' : undefined}
	onclick={selectionClick} onkeydown={selectionKey}
	ondragover={dragover} ondrop={drop}
	ondragleave={(event) => { if (!(event.relatedTarget instanceof Node) || !list?.contains(event.relatedTarget)) dropRow = null; }}
>
	<!-- Stable track identity preserves keyboard focus through reorders. The
		store guarantees uniqueness: addTrack dedupes and restoreQueue rejects duplicates. -->
	{#each visibleEntries as { track, index } (track.id)}
		<!-- svelte-ignore a11y_no_noninteractive_tabindex a11y_no_noninteractive_element_interactions: rows are programmatic targets, not tab stops; modifier-click selects without activating controls. -->
		<li
			class="queue-row"
			class:drop-before={dropRow === track.id && !dropAfter}
			class:drop-after={dropRow === track.id && dropAfter}
			use:contextMenu={!interactionsUnavailable && (oninsert || onreorder) ? (invoker, point) => openTrackMenu(track.id, invoker, point) : undefined}
			data-testid="queue-item"
			data-track-id={track.id}
			data-selected={batchIds.includes(track.id) ? '' : undefined}
			aria-current={index === selectedIndex ? 'true' : undefined}
			tabindex={index === selectedIndex ? -1 : undefined}
		>
			{#if selection && selectionControls}
				<label class="queue-selection">
					<input type="checkbox" aria-label="Select {displayTitle(track)} for queue actions"
						checked={selection.ids.includes(track.id)}
						onclick={(event) => {
							if (!event.currentTarget.isConnected || !list?.contains(event.currentTarget)) return;
							const selected = selectionControls?.toggleQueueSelection(track.id, event.shiftKey);
							event.currentTarget.checked = selected?.ids.includes(track.id) ?? false;
						}}
						onkeydown={(event) => {
							if (event.key !== ' ' || !event.shiftKey || event.defaultPrevented || event.repeat ||
								event.isComposing || event.altKey || event.ctrlKey || event.metaKey ||
								!event.currentTarget.isConnected || !list?.contains(event.currentTarget)) return;
							event.preventDefault();
							selectionControls?.toggleQueueSelection(track.id, true);
						}} />
					Select
				</label>
			{/if}
			{#if onreorder}
				<button type="button" class="queue-grip" draggable="true"
					use:pointerQueueDrag={{
						ids: () => interactionsUnavailable ? [] : batchFor(track.id), list: () => list, version: dragVersion,
						hover: (target, clientY) => { dropDestination({ target, clientY }); },
						drop: (ids, target, clientY) => { onreorder?.(ids, dropDestination({ target, clientY }, ids)); dropRow = null; },
						cancel: () => { dropRow = null; }
					}}
					aria-label="Reorder {displayTitle(track)}" aria-haspopup="menu"
					onpointerdown={gripDown} onpointermove={gripMove}
					onclick={(event) => gripClick(event, track.id)}
					ondragstart={(event) => startTrackDrag(event, interactionsUnavailable ? [] : batchFor(track.id), list)}
				>Reorder</button>
			{/if}
			{displayTitle(track)}{#if track.artist}{' — '}{track.artist}{/if}
			{#if oninsert || onreorder}
				<button type="button" class="queue-more" data-testid="queue-more" aria-haspopup="menu"
					aria-label="More actions for {displayTitle(track)}" aria-disabled={interactionsUnavailable}
					onclick={(event) => openTrackMenu(track.id, event.currentTarget)}
				><MoreActionsGlyph /></button>
			{/if}
			{#if ontogglefavorite}
				<button
					type="button"
					class="queue-star"
					aria-label="{favoriteIds.has(track.id) ? 'Unstar' : 'Star'} {displayTitle(track)}"
					aria-pressed={favoriteIds.has(track.id)}
					onclick={() => ontogglefavorite?.(track.id)}
				>
					<span aria-hidden="true">{favoriteIds.has(track.id) ? '★' : '☆'}</span>
				</button>
			{/if}
			{#if oncopylink}
				<button
					type="button"
					class="queue-copy-link"
					data-testid="queue-copy-link"
					aria-label="Copy link {displayTitle(track)}"
					onclick={() => oncopylink?.(track.id)}
				>
					Copy link
				</button>
			{/if}
			{#each trackCount > 1 ? ['Up', 'Down', 'Top', 'Bottom'] : ['Up', 'Down'] as move}
				{@const target = move === 'Up' ? index - 1 : move === 'Down' ? index + 1 : move === 'Top' ? 0 : trackCount - 1}
				<button
					type="button"
					aria-label="Move {displayTitle(track)} {move === 'Top' || move === 'Bottom' ? 'to ' : ''}{move.toLowerCase()}"
					disabled={target < 0 || target >= trackCount || target === index}
					onclick={() => onmove?.(index, target)}
				>
					{move}
				</button>
			{/each}
			<button
				type="button"
				aria-label="Play {displayTitle(track)} next"
				onclick={() => onplaynext?.(index)}
			>
				Next
			</button>
			<button
				type="button"
				aria-label="Remove {displayTitle(track)} from queue"
				onclick={(event) => void reportRemove(event, index)}
			>
				Remove
			</button>
			{#if onselect}
				<button
					type="button"
					aria-label="Select {displayTitle(track)}"
					onclick={(event) => reportSelect(event, index, track.id)}
				>
					Select
				</button>
			{/if}
		</li>
	{/each}
</ol>

{#if onkeepcurrentandupcoming}
	<button
		type="button"
		disabled={selectedIndex <= 0}
		onclick={(event) => void reportTrim(event, onkeepcurrentandupcoming)}
	>
		Keep current and upcoming
	</button>
{/if}
{#if onclearupcoming}
	<button
		type="button"
		disabled={selectedIndex < 0 || selectedIndex >= trackCount - 1}
		onclick={requestTrim}
	>
		Clear upcoming
	</button>
{/if}

{#if trackCount === 0}
	<!--
		Stable recovery target when no rows survive: a programmatically
		focusable status line, never a control, tab stop or modal. Focused
		only by explicit remove/clear recovery, never on render.
	-->
	<p data-testid="queue-empty-focus" tabindex="-1" bind:this={emptyTarget}>Queue is empty</p>
{/if}

<button
	type="button"
	disabled={queue.tracks.length === 0}
	onclick={requestClear}
>
	Clear queue
</button>

{#if pending !== null && pendingValid}
	<!-- A fieldset keeps the prompt a labelled group without a dialog trap:
		the rest of the page stays reachable and nothing is modal. -->
	<fieldset class="queue-confirm" data-testid="queue-clear-confirm" bind:this={confirmEl}>
		<legend>{pending === 'clear' ? 'Clear queue?' : 'Clear upcoming?'}</legend>
		<p id="{confirmId}-message" aria-live="polite" aria-atomic="true">
			{#if pending === 'clear'}
				Remove all {trackCount} {trackCount === 1 ? 'track' : 'tracks'} from the queue? This cannot be undone.
			{:else}
				Remove {upcoming} upcoming {upcoming === 1 ? 'track' : 'tracks'} from the queue? The current track stays. This cannot be undone.
			{/if}
		</p>
		<div class="queue-confirm-actions">
			<button
				type="button"
				bind:this={cancelButtonEl}
				aria-describedby="{confirmId}-message"
				onclick={() => void cancelConfirm()}
			>
				Cancel
			</button>
			<button type="button" aria-describedby="{confirmId}-message" onclick={(event) => void acceptConfirm(event)}>
				{pending === 'clear' ? 'Yes, clear queue' : 'Yes, clear upcoming'}
			</button>
		</div>
	</fieldset>
{/if}

<!-- Polite outcome for a cancelled confirm; the queue-announcement region
	stays reserved for real queue changes. -->
<p data-testid="queue-confirm-feedback" aria-live={liveRegions ? 'polite' : undefined}>{#key cancelNoteVersion}{cancelNote}{/key}</p>

{#if trackMenu}
	{#key trackMenu}
		<ContextMenu request={trackMenu} onclose={() => (trackMenu = null)} />
	{/key}
{/if}

<style>
	ol { min-height: 48px; }
	.queue-grip { cursor: grab; min-height: 40px; touch-action: none; }
	.queue-more { min-width: 40px; min-height: 40px; }
	.queue-more[aria-disabled='true'] { color: var(--nt-fg-muted); cursor: default; background: transparent; }
	.queue-row[data-selected] { background: var(--nt-bg-raised); outline: 1px solid var(--nt-accent); }
	@media (pointer: coarse) {
		.queue-grip, .queue-more { min-width: 48px; min-height: 48px; }
	}
	:global(html[data-track-dragging='library']) ol[data-accepts-tracks] {
		outline: 2px dashed var(--nt-accent);
	}
	:global(html[data-track-dragging]) .drop-before { box-shadow: 0 -2px var(--nt-accent); }
	:global(html[data-track-dragging]) .drop-after { box-shadow: 0 2px var(--nt-accent); }

	.queue-filter {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: var(--nt-space-2);
	}

	.queue-filter input {
		flex: 1 1 12rem;
		min-width: 0;
		max-width: 100%;
		min-height: 2.75rem;
		padding: var(--nt-space-2) var(--nt-space-3);
		font: inherit;
		color: var(--nt-fg);
		background: var(--nt-bg-raised);
		border: 1px solid var(--nt-border);
		border-radius: var(--nt-radius);
	}

	/* Visible keyboard focus ring even where the app stylesheet is absent
	 * (mounted test parent); the app's own :focus-visible agrees with this. */
	.queue-filter input:focus-visible {
		outline: 2px solid var(--nt-accent);
		outline-offset: 2px;
	}

	/* Touch-sized clear action matching the input row. */
	.queue-filter button {
		min-height: 2.75rem;
		padding: var(--nt-space-2) var(--nt-space-3);
		font: inherit;
	}

	.queue-selection {
		display: inline-flex;
		align-items: center;
		gap: var(--nt-space-2);
		min-height: 2.75rem;
		padding-inline: var(--nt-space-2);
		cursor: pointer;
	}

	.queue-selection input:focus-visible {
		outline: 2px solid var(--nt-accent, currentColor);
		outline-offset: 2px;
	}

	.queue-star {
		min-width: 2.75rem;
		min-height: 2.75rem;
		font: inherit;
		color: inherit;
		cursor: pointer;
		vertical-align: middle;
	}

	.queue-copy-link {
		min-height: 2.75rem;
		font: inherit;
		cursor: pointer;
	}

	.queue-star:focus-visible,
	.queue-copy-link:focus-visible {
		outline: 2px solid var(--nt-accent, currentColor);
		outline-offset: 2px;
	}

	.queue-confirm {
		min-width: 0;
		margin-inline: 0;
		padding: var(--nt-space-2) var(--nt-space-3);
		color: var(--nt-fg);
		background: var(--nt-bg-raised);
		border: 1px solid var(--nt-border);
		border-radius: var(--nt-radius);
	}

	.queue-confirm p {
		overflow-wrap: anywhere;
	}

	.queue-confirm-actions {
		display: flex;
		flex-wrap: wrap;
		gap: var(--nt-space-2) var(--nt-space-3);
	}

	/* Touch-sized, with a ring that holds without the app stylesheet. */
	.queue-confirm button {
		min-height: 2.75rem;
		padding: var(--nt-space-2) var(--nt-space-3);
		font: inherit;
	}

	.queue-confirm button:focus-visible {
		outline: 2px solid var(--nt-accent, currentColor);
		outline-offset: 2px;
	}

	.queue-row {
		overflow-wrap: anywhere;
		/* Clear the 2px outline + 2px offset when nearest-scroll rounds
		 * fractional line heights at the viewport edge. */
		scroll-margin-block: 4px;
	}

	/* Programmatic focus target for explicit reveal: the ring must hold
	 * even where the app stylesheet is absent (mounted test parent). */
	.queue-row:focus {
		outline: 2px solid var(--nt-accent, currentColor);
		outline-offset: 2px;
	}

	/* Same ring for the empty-queue and zero-visible recovery targets:
	 * keyboard recovery must stay visible without relying on the host
	 * page's stylesheet. */
	[data-testid='queue-empty-focus'],
	[data-testid='queue-filter-empty'] {
		scroll-margin-block: 4px;
	}

	[data-testid='queue-empty-focus']:focus,
	[data-testid='queue-filter-empty']:focus {
		outline: 2px solid var(--nt-accent, currentColor);
		outline-offset: 2px;
	}
</style>
