<script lang="ts">
	import { ICON_PATHS, type IconName } from './icons.js';
	import IconSvg from './IconSvg.svelte';

	interface Props {
		name: IconName;
		/** Actionable icons take the visible action name here. Decorative
		 * icons omit it and render aria-hidden. */
		label?: string;
		size?: number | string;
	}

	let { name, label, size = 24 }: Props = $props();
</script>

<IconSvg path={ICON_PATHS[name]} {label} {size} />
