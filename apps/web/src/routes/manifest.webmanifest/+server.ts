import type { RequestHandler } from './$types';

// PWA install manifest. A server route (not a static file) guarantees the
// `application/manifest+json` content type on every host. theme_color
// #0b0d12 matches --nt-bg (dark default) in src/lib/design/tokens.css; the
// icons are generated PNGs in static/icons. Pinned by
// apps/web/tests/pwa-manifest.spec.ts — keep the contract in sync.
const MANIFEST = {
	name: 'NNTune',
	short_name: 'NNTune',
	description:
		'NNTune streams your music on Cloudflare. Bring your own Usenet provider; we provide search, streaming and library.',
	start_url: '/',
	scope: '/',
	display: 'standalone',
	background_color: '#0b0d12',
	theme_color: '#0b0d12',
	icons: [
		{
			src: '/icons/icon-192.png',
			sizes: '192x192',
			type: 'image/png'
		},
		{
			src: '/icons/icon-512.png',
			sizes: '512x512',
			type: 'image/png'
		}
	]
} as const;

export const GET: RequestHandler = () =>
	new Response(JSON.stringify(MANIFEST), {
		headers: {
			'content-type': 'application/manifest+json',
			'cache-control': 'public, max-age=3600'
		}
	});
