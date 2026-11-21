<script lang="ts">
	import type { Action } from 'svelte/action';
	import type { createCurrentTrackFavorite } from './currentTrackFavorite.svelte.js';
	import type { QueueState } from './queue.js';
	let { currentFavorite, snapshot }: { currentFavorite: ReturnType<typeof createCurrentTrackFavorite>; snapshot: QueueState } = $props();
	const target = $derived(currentFavorite.view(snapshot));
	const feedback = $derived(currentFavorite.feedback(target));
	$effect(() => currentFavorite.reconcileFeedback(target));
	const activateRendered: Action<HTMLButtonElement, NonNullable<typeof target>> = (button, rendered) => {
		const listeners = new AbortController();
		const capture = { capture: true, signal: listeners.signal };
		const hits = (event: Event) => event.composedPath().includes(button);
		// Touch and pen clicks must match the originating pointer.
		const pointers = new Map<string, { id: number; intent: typeof rendered }>();
		const takePointer = ({ pointerId, pointerType }: PointerEvent) => {
			const press = pointers.get(pointerType);
			if (press?.id !== pointerId) return;
			pointers.delete(pointerType);
			return press.intent;
		};
		let mouse: typeof rendered | undefined, mouseRelease: typeof rendered | undefined;
		let space: typeof rendered | undefined, legacyRelease: PointerEvent | undefined;
		let release: ReturnType<typeof setTimeout> | undefined;
		const clearSpace = () => { clearTimeout(release); release = space = undefined; };
		const intents = new WeakMap<Event, typeof rendered>();
		window.addEventListener('pointerdown', (event) => {
			if (!event.isPrimary || event.button !== 0) return;
			if (event.isTrusted) mouseRelease = legacyRelease = undefined;
			if (event.pointerType === 'mouse') mouse = hits(event) ? rendered : undefined;
			else if (hits(event)) pointers.set(event.pointerType, { id: event.pointerId, intent: rendered });
			else takePointer(event);
		}, capture);
		window.addEventListener('pointercancel', (event) => {
			if (event.pointerType === 'mouse') mouse = undefined; else takePointer(event);
		}, capture);
		// Chords emit mouse button events without another pointerdown/pointerup.
		window.addEventListener('mousedown', (event) => {
			if (event.isTrusted && event.button === 0 && (event.buttons !== 1 || !mouse)) mouse = hits(event) ? rendered : undefined;
		}, capture);
		window.addEventListener('mouseup', (event) => {
			if (!event.isTrusted || event.button !== 0) return;
			mouseRelease = hits(event) ? mouse : undefined;
			mouse = undefined;
		}, capture);
		window.addEventListener('pointerup', (event) => {
			if (event.isTrusted && event.isPrimary && event.button === 0) legacyRelease = hits(event) ? event : undefined;
		}, capture);
		window.addEventListener('keydown', (event) => {
			if (event.key === ' ' && !event.repeat && hits(event)) { clearSpace(); space = rendered; }
		}, capture);
		button.addEventListener('blur', clearSpace, { signal: listeners.signal });
		window.addEventListener('keyup', (event) => {
			// Space clicks after keyup; expire a press whose click was canceled.
			if (event.key === ' ' && space) release = setTimeout(clearSpace, 0);
		}, capture);
		window.addEventListener('click', (event) => {
			const fromPointer = event.isTrusted && (event.detail > 0 || ('pointerType' in event && !!event.pointerType));
			const fromSpace = event.isTrusted && !fromPointer && release !== undefined;
			const pointer = 'pointerType' in event ? event as PointerEvent : legacyRelease;
			const intent = fromPointer ? (pointer && pointer.pointerType !== 'mouse' ? takePointer(pointer) : mouseRelease)
				: fromSpace ? space : undefined;
			if (fromPointer) mouseRelease = legacyRelease = undefined;
			if (fromSpace) clearSpace();
			if (hits(event)) intents.set(event, intent ?? rendered);
		}, capture);
		button.addEventListener('click', (event) => {
			const intent = intents.get(event);
			intents.delete(event);
			if (intent && !event.defaultPrevented) void currentFavorite.activate(intent);
		}, { signal: listeners.signal });
		return { update(next) { rendered = next; }, destroy() { clearSpace(); listeners.abort(); } };
	};
</script>
{#if target && !target.loading}
	<button type="button" class="nt-button nt-button--ghost" data-testid="player-favorite"
		aria-label={`Favorite ${target.title}`} aria-pressed={target.pressed} use:activateRendered={target}>
		<span aria-hidden="true">{target.pressed ? '★' : '☆'}</span> Favorite
	</button>
{/if}
<p aria-live="polite" aria-atomic="true" data-testid="player-favorite-status">{#if feedback}{#key feedback.edit}{feedback.text}{/key}{/if}</p>
<style>
	button { min-height: 2.75rem; }
	p { margin: 0; overflow-wrap: anywhere; }
	p:empty { position: absolute; }
</style>
