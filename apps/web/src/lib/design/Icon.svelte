<script lang="ts">
	import { ICON_PATHS, type IconName } from './icons.js';

	interface Props {
		name: IconName;
		/** Actionable icons take the visible action name here. Decorative
		 * icons omit it and render aria-hidden. */
		label?: string;
		size?: number | string;
	}

	let { name, label, size = 24 }: Props = $props();
</script>

<svg
	viewBox="0 0 24 24"
	width={size}
	height={size}
	fill="none"
	stroke="currentColor"
	stroke-width="2"
	stroke-linecap="round"
	stroke-linejoin="round"
	focusable="false"
	role={label ? 'img' : undefined}
	aria-label={label}
	aria-hidden={label ? undefined : 'true'}
>
	{@html ICON_PATHS[name]}
</svg>
