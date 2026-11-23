<script lang="ts">
	import { onDestroy } from 'svelte';
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

	const headingId = $props.id();
	let dialog: HTMLDialogElement;
	let slot: HTMLDivElement;
	let status = $state('');
	let stop: (() => void) | undefined;

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

		if (!fits()) {
			status = 'The video player needs more room.';
			close();
			return;
		}

		let frame: HTMLIFrameElement | undefined;
		let player: YouTubePlayer | undefined;
		let offAudio: (() => void) | undefined;
		const listeners = new AbortController();
		const { signal } = listeners;
		const observer = new IntersectionObserver(([entry]) => {
			if (entry.intersectionRatio < 1 || !fits()) {
				close();
			} else if (!frame) {
				void openPlayer();
			}
		}, { threshold: 1 });
		const offPaywall = onPaywall(close);
		const timeout = window.setTimeout(unavailable, 12_000);
		let requested = false;
		stop = () => {
			stop = undefined;
			listeners.abort();
			observer.disconnect();
			offPaywall();
			offAudio?.();
			window.clearTimeout(timeout);
			try {
				player?.pauseVideo();
				player?.destroy();
			} catch {
				// The frame still has to be removed if the API has failed.
			} finally {
				frame?.remove();
			}
		};
		observer.observe(slot);
		document.addEventListener('visibilitychange', () => { if (document.hidden) close(); }, { signal });
		window.addEventListener('pagehide', close, { signal });
		function checkSize() { if (!fits()) close(); }
		window.addEventListener('resize', checkSize, { signal });
		viewport?.addEventListener('resize', checkSize, { signal });
		viewport?.addEventListener('scroll', checkSize, { signal });

		function unavailable() {
			if (signal.aborted) return;
			status = 'The video is unavailable.';
			close();
		}

		async function openPlayer() {
			if (requested) return;
			requested = true;
			try {
				const api = await loadYouTubeApi();
				if (signal.aborted) return;
				if (!fits() || getVideoId() !== videoId) return close();
				pauseAudio();
				offAudio = audioStatus.subscribe((state) => {
					if (state === 'playing' || state === 'starting' || state === 'retrying') close();
				});
				if (signal.aborted) {
					offAudio();
					return;
				}
				const embed = new URL(`/embed/${videoId}`, YOUTUBE_EMBED_ORIGIN);
				embed.search = new URLSearchParams({ enablejsapi: '1', origin: window.location.origin }).toString();
				frame = document.createElement('iframe');
				frame.title = `${title} on YouTube`;
				frame.referrerPolicy = 'strict-origin-when-cross-origin';
				frame.allow = "autoplay; fullscreen; picture-in-picture 'none'";
				frame.src = embed.href;
				slot.append(frame);
				player = new api.Player(frame, { events: {
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

	onDestroy(() => stop?.());
</script>

<button type="button" class="nt-button nt-button--ghost" aria-haspopup="dialog" onclick={watch}>Watch video</button>
<a href="https://www.youtube.com/t/terms" rel="noreferrer">YouTube Terms of Service</a>
<a href="https://policies.google.com/privacy" rel="noreferrer">Google Privacy Policy</a>
<p role="status">{status}</p>

<dialog bind:this={dialog} aria-labelledby={headingId} oncancel={() => stop?.()}
	onclose={() => { if (!dialog.open) stop?.(); }}>
	<h3 id={headingId}>{title}</h3>
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
		background: black;
	}
	.video-slot :global(iframe) {
		display: block;
		inline-size: 100%;
		block-size: 100%;
		border: 0;
	}
</style>
