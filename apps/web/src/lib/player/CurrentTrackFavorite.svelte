<script lang="ts">
	import type { Action } from 'svelte/action';
	import type { createCurrentTrackFavorite } from './currentTrackFavorite.svelte.js';
	import type { QueueState } from './queue.js';

	let { currentFavorite, snapshot }: {
		currentFavorite: ReturnType<typeof createCurrentTrackFavorite>;
		snapshot: QueueState;
	} = $props();
	const target = $derived(currentFavorite.view(snapshot));
	const feedback = $derived(currentFavorite.feedback(target));

	// Pin the rendered intent at click entry; another handler can flush a
	// selection or membership change before the event reaches the button.
	const activateRendered: Action<HTMLButtonElement, NonNullable<typeof target>> = (button, rendered) => {
		const intents = new WeakMap<Event, typeof rendered>();
		const capture = (event: Event) => {
			if (event.composedPath().includes(button)) intents.set(event, rendered);
		};
		const click = (event: Event) => {
			const intent = intents.get(event);
			intents.delete(event);
			if (intent && !event.defaultPrevented) currentFavorite.activate(intent);
		};
		window.addEventListener('click', capture, true);
		button.addEventListener('click', click);
		return {
			update(next) { rendered = next; },
			destroy() {
				window.removeEventListener('click', capture, true);
				button.removeEventListener('click', click);
			}
		};
	};
</script>

{#if target}
	<button type="button" class="nt-button nt-button--ghost" data-testid="player-favorite"
		aria-label={`Favorite ${target.title}`} aria-pressed={target.pressed} use:activateRendered={target}>
		<span aria-hidden="true">{target.pressed ? '★' : '☆'}</span> Favorite
	</button>
{/if}
<p aria-live="polite" aria-atomic="true" data-testid="player-favorite-status">{#if feedback}{#key feedback.version}{feedback.text}{/key}{/if}</p>

<style>
	button { min-height: 2.75rem; }
	p { margin: 0; overflow-wrap: anywhere; }
	p:empty { position: absolute; }
</style>
