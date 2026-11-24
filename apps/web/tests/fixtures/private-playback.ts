import type { Page } from '@playwright/test';

const binding = 'A'.repeat(43);

export async function mockPrivatePlayback(page: Page) {
	const env = { PUBLIC_API_BASE_URL: '', PUBLIC_BROWSER_PLAYBACK: true };
	await page.route('**/_app/env.js', (route) => route.fulfill({ contentType: 'application/javascript', body: `export const env=${JSON.stringify(env)}` }));
	await page.route('**/library', async (route) => {
		const response = await route.fetch();
		const body = (await response.text()).replace(/("?PUBLIC_BROWSER_PLAYBACK"?\s*:\s*)false/g, '$1true');
		await route.fulfill({ response, body });
	});
	await page.route('**/api/library', (route) => route.fulfill({
		headers: { 'x-nntune-session-binding': binding }, json: [{ id: 'first', title: 'Fixture track' }]
	}));
	await page.route('**/v1/me/likes**', (route) => route.fulfill({ json: { session_binding: binding, items: [] } }));
	await page.route('**/v1/plays', (route) => {
		const { track_id, session_id } = route.request().postDataJSON();
		return route.fulfill({ json: { trackId: track_id, playbackSessionId: session_id,
			playId: 'sample', durationMs: 120_000, sessionBinding: binding } });
	});
	await page.route('**/v1/plays/media/**', (route) => route.abort());
}
