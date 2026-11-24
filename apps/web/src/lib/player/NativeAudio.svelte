<script lang="ts">
	let { mediaRevision, streamUrl, attachNativeAudio }: {
		mediaRevision: number;
		streamUrl: string;
		attachNativeAudio: (audio: HTMLAudioElement) => () => void;
	} = $props();
</script>

{#key mediaRevision}
	<audio {@attach attachNativeAudio} data-testid="audio-player" controls src={streamUrl}></audio>
{/key}

<style>
	audio {
		max-width: 100%;
		flex: 1 1 12rem;
		min-width: 0;
	}
</style>
