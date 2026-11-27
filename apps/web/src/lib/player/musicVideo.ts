export const YOUTUBE_EMBED_ORIGIN = 'https://www.youtube-nocookie.com';
export const YOUTUBE_API_ORIGIN = 'https://www.youtube.com';

const verificationLifetime = 30 * 24 * 60 * 60 * 1000;

export interface VerifiedVideoLink {
	recordingId: string;
	videoId: string;
	// Timestamp of a successful public, embeddable, unrestricted batch check.
	verifiedAt: number;
}

export function verifiedVideoId(link: VerifiedVideoLink, recordingId: string, now = Date.now()) {
	const age = now - link.verifiedAt;
	if (!recordingId || link.recordingId !== recordingId || !Number.isFinite(now) ||
		!Number.isFinite(link.verifiedAt) || age < 0 || age >= verificationLifetime ||
		typeof link.videoId !== 'string' || !/^[A-Za-z0-9_-]{11}$/.test(link.videoId)) {
		return null;
	}
	return link.videoId;
}

export interface YouTubePlayer {
	playVideo(): void;
	pauseVideo(): void;
	destroy(): void;
}

interface YouTubeApi {
	Player: new (frame: HTMLIFrameElement, options: {
		host: string;
		events: {
			onReady: (event: { target: YouTubePlayer }) => void;
			onError: () => void;
		};
	}) => YouTubePlayer;
}

let loadingApi: Promise<YouTubeApi> | undefined;

export function loadYouTubeApi() {
	const host = window as Window & { YT?: YouTubeApi; onYouTubeIframeAPIReady?: () => void };
	if (typeof host.YT?.Player === 'function') return Promise.resolve(host.YT);
	loadingApi ??= new Promise<YouTubeApi>((resolve, reject) => {
		const script = document.createElement('script');
		const timer = window.setTimeout(fail, 8_000);
		let settled = false;
		function cleanup() {
			settled = true;
			window.clearTimeout(timer);
			script.onerror = null;
			if (host.onYouTubeIframeAPIReady === ready) delete host.onYouTubeIframeAPIReady;
		}
		function fail() {
			if (settled) return;
			cleanup();
			script.remove();
			reject(new Error('YouTube player API unavailable'));
		}
		function ready() {
			if (settled) return;
			const api = host.YT;
			if (typeof api?.Player !== 'function') return fail();
			cleanup();
			resolve(api);
		}
		try {
			host.onYouTubeIframeAPIReady = ready;
			script.onerror = fail;
			script.src = `${YOUTUBE_API_ORIGIN}/iframe_api`;
			document.head.append(script);
		} catch {
			fail();
		}
	}).catch((error: unknown) => {
		loadingApi = undefined;
		throw error;
	});
	return loadingApi;
}
