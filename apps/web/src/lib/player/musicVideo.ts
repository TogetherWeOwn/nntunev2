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
	if (link.recordingId !== recordingId || !Number.isFinite(age) || age < 0 ||
		age >= verificationLifetime || link.videoId.length !== 11 || !/^[A-Za-z0-9_-]+$/.test(link.videoId)) {
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
	if (host.YT?.Player) return Promise.resolve(host.YT);
	loadingApi ??= new Promise<YouTubeApi>((resolve, reject) => {
		const script = document.createElement('script');
		const timer = window.setTimeout(fail, 8_000);
		function fail() {
			window.clearTimeout(timer);
			script.onerror = null;
			delete host.onYouTubeIframeAPIReady;
			script.remove();
			loadingApi = undefined;
			reject(new Error('YouTube player API unavailable'));
		}
		host.onYouTubeIframeAPIReady = () => {
			if (!host.YT?.Player) return fail();
			window.clearTimeout(timer);
			script.onerror = null;
			delete host.onYouTubeIframeAPIReady;
			resolve(host.YT);
		};
		script.onerror = fail;
		script.src = `${YOUTUBE_API_ORIGIN}/iframe_api`;
		document.head.append(script);
	});
	return loadingApi;
}
