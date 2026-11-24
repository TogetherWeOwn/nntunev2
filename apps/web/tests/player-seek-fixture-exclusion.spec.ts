import { expect, test } from '@playwright/test';
import { ARTWORK_PATH, AUDIO_PATH } from './fixtures/player-seek/paths.js';

test('the app build does not serve the isolated player fixture or audio', async ({ request }) => {
	for (const path of [
		'/tests/fixtures/player-seek/index.html',
		'/fixtures/synthetic-audio/player-seek/seek-fixture.wav',
		AUDIO_PATH,
		ARTWORK_PATH
	]) {
		const response = await request.get(path, { maxRedirects: 0 });
		expect(response.status(), path).toBe(404);
	}
});
