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

	// Keep the intent paired with what's rendered, not with a lazy read of
	// the latest selection during a click dispatched before the DOM updates.
	const activateRendered: Action<HTMLButtonElement, NonNullable<typeof target>> = (button, rendered) => {
		const click = () => currentFavorite.activate(rendered);
		button.addEventListener('click', click);
		return {
			update(next) { rendered = next; },
			destroy() { button.removeEventListener('click', click); }
		};
	};
</script>

{#if target}
	<button type="button" class="nt-button nt-button--ghost" data-testid="player-favorite"
		aria-label={`Favorite ${target.title}`} aria-pressed={target.pressed} use:activateRendered={target}>
		<span aria-hidden="true">{target.pressed ? '★' : '☆'}</span> Favorite
	</button>
{/if}
<p role="status" aria-atomic="true" data-testid="player-favorite-status">{#if feedback}{#key feedback.version}{feedback.text}{/key}{/if}</p>

<style>
	button { min-height: 2.75rem; }
	p { margin: 0; overflow-wrap: anywhere; }
	p:empty { position: absolute; }
</style>
