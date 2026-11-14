<script lang="ts">
	/** Internal renderer for trusted kit stencil data, never user content. */
	let { path, label, size = 24 }: { path: string; label?: string; size?: number | string } = $props();
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
	{@html path}
</svg>
