<script lang="ts">
	import { onMount } from 'svelte';
	import type { Readable } from 'svelte/store';
	import { onPaywall } from '#lib/billing/planLimit.js';
	import type { PlaybackStatus } from './mediaController.js';
	import { loadYouTubeApi, YOUTUBE_EMBED_ORIGIN, type YouTubePlayer } from './musicVideo.js';

	let { title, getVideoId, pauseAudio, audioStatus }: {
		title: string;
		getVideoId: () => string | null;
		pauseAudio: () => void;
		audioStatus: Readable<PlaybackStatus>;
	} = $props();

	let dialog: HTMLDialogElement;
	let slot: HTMLDivElement;
	let status = $state('');
	let stop: (() => void) | undefined;

	const audioActive = $derived(['playing', 'starting', 'retrying'].includes($audioStatus));
	$effect.pre(() => { if (audioActive && stop && dialog.open && slot.firstChild) close(); });

	function close() {
		stop?.();
		dialog.close();
	}

	function watch() {
		const videoId = getVideoId();
		if (!videoId || document.hidden || document.querySelector('dialog:modal')) return;
		status = '';
		dialog.showModal();

		const viewport = window.visualViewport;
		function fits() {
			const { left, top, right, bottom, width, height } = slot.getBoundingClientRect();
			const x = viewport?.offsetLeft ?? 0;
			const y = viewport?.offsetTop ?? 0;
			return dialog.open && !document.hidden && width >= 200 && height >= 200 &&
				left >= x && top >= y &&
				right <= x + (viewport?.width ?? document.documentElement.clientWidth) &&
				bottom <= y + (viewport?.height ?? document.documentElement.clientHeight);
		}

		function tooSmall() {
			status = 'The video player needs more room.';
			close();
		}
		if (!fits()) return tooSmall();

		let frame: HTMLIFrameElement | undefined;
		let player: YouTubePlayer | undefined;
		const listeners = new AbortController();
		const { signal } = listeners;
		let started = false;
		const observer = new IntersectionObserver((entries) => {
			if (signal.aborted) return;
			if (entries.at(-1)?.intersectionRatio !== 1 || !fits()) tooSmall();
			else if (!started) void openPlayer();
		}, { threshold: 1 });
		const timeout = window.setTimeout(unavailable, 12_000);
		stop = () => {
			stop = undefined;
			listeners.abort();
			observer.disconnect();
			window.clearTimeout(timeout);
			try { player?.pauseVideo?.(); } catch { /* The player can still be starting. */ }
			try { player?.destroy(); } catch { /* Removing the frame still stops playback. */ }
			frame?.remove();
		};
		observer.observe(slot);
		document.addEventListener('visibilitychange', () => { if (document.hidden) close(); }, { signal });
		window.addEventListener('pagehide', close, { signal });
		function checkSize() { if (!fits()) close(); }
		(viewport ?? window).addEventListener('resize', checkSize, { signal });
		viewport?.addEventListener('scroll', checkSize, { signal });

		function unavailable() {
			if (signal.aborted) return;
			status = 'The video is unavailable.';
			close();
		}

		async function openPlayer() {
			started = true;
			try {
				const api = await loadYouTubeApi();
				if (signal.aborted) return;
				if (!fits() || getVideoId() !== videoId) return close();
				pauseAudio();
				if (audioActive) return close();
				const embed = new URL(`/embed/${videoId}`, YOUTUBE_EMBED_ORIGIN);
				embed.search = new URLSearchParams({ enablejsapi: '1', origin: window.location.origin }).toString();
				frame = document.createElement('iframe');
				frame.title = `${title} on YouTube`;
				frame.referrerPolicy = 'strict-origin-when-cross-origin';
				frame.allow = "autoplay; fullscreen; picture-in-picture 'none'";
				frame.src = embed.href;
				slot.append(frame);
				player = new api.Player(frame, { host: YOUTUBE_EMBED_ORIGIN, events: {
					onReady: ({ target }) => {
						if (signal.aborted) return;
						if (!fits() || getVideoId() !== videoId) return close();
						window.clearTimeout(timeout);
						target.playVideo();
					},
					onError: unavailable
				} });
			} catch {
				unavailable();
			}
		}
	}

	onMount(() => {
		const offPaywall = onPaywall(() => { if (dialog.open) close(); });
		return () => { offPaywall(); stop?.(); };
	});
</script>

<button type="button" class="nt-button nt-button--ghost" aria-haspopup="dialog" onclick={watch}>Watch video</button>
<a href="https://www.youtube.com/t/terms" rel="noreferrer">YouTube Terms of Service</a>
<a href="https://policies.google.com/privacy" rel="noreferrer">Google Privacy Policy</a>
<p role="status">{status}</p>

<dialog bind:this={dialog} aria-label={title} oncancel={() => stop?.()}
	onclose={() => { if (!dialog.open) stop?.(); }}>
	<form method="dialog" onsubmit={() => stop?.()}>
		<button type="submit" class="nt-button nt-button--ghost">Close video</button>
	</form>
	<div class="video-slot" bind:this={slot}></div>
</dialog>

<style>
	dialog {
		box-sizing: content-box;
		inline-size: min(480px, calc(100% - 3rem));
		padding: var(--nt-space-3);
		border: 0;
		border-radius: var(--nt-radius);
		background: var(--nt-bg);
		color: var(--nt-fg);
	}
	dialog:modal { overscroll-behavior: contain; }
	dialog::backdrop {
		background: rgb(0 0 0 / 70%);
		overflow: hidden;
		overscroll-behavior: contain;
	}
	.video-slot {
		margin-block-start: var(--nt-space-3);
		aspect-ratio: 16 / 9;
		min-block-size: 200px;
		max-block-size: max(200px, calc(100dvh - 8rem));
	}
	.video-slot :global(iframe) {
		display: block;
		inline-size: 100%;
		block-size: 100%;
		border: 0;
	}
</style>
