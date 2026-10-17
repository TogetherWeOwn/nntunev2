import { expect, test, type Page } from '@playwright/test';

const MEDIA_BYTES = 256 * 1024;
// Earlier storm offsets are aborted; inject on the final, non-superseded request.
const RANGE_REQUEST_PATTERN = /^bytes=(\d+)-(\d+)$/;
const FINAL_STORM_RANGE = 'bytes=9000-9099';

function mismatchedRange(start: number, end: number) {
	const mismatchedStart = start + 1;
	return { start: mismatchedStart, end: mismatchedStart + end - start };
}

async function runSeekStormWithInjectedResponse(page: Page, status: 200 | 206) {
	await page.goto('/demo/player');
	await page.route(/\/api\/demo\/audio(?:\?.*)?$/, async (route) => {
		const range = route.request().headers()['range'];
		const match = range?.match(RANGE_REQUEST_PATTERN);
		if (range !== FINAL_STORM_RANGE || !match) {
			await route.continue();
			return;
		}

		const start = Number(match[1]);
		const end = Number(match[2]);
		const mismatched = mismatchedRange(start, end);
		const headers = {
			'x-test-injected-range': 'true',
			...(status === 206
				? { 'content-range': `bytes ${mismatched.start}-${mismatched.end}/${MEDIA_BYTES}` }
				: {})
		};

		await route.fulfill({
			status,
			contentType: 'audio/mpeg',
			headers,
			body: String.fromCharCode(0).repeat(status === 200 ? MEDIA_BYTES : end - start + 1)
		});
	});

	const response = page.waitForResponse(
		(response) => response.headers()['x-test-injected-range'] === 'true',
		{ timeout: 15_000 }
	);
	await page.getByRole('button', { name: 'Run 10x seek storm' }).click();
	const injectedResponse = await response;
	const requestedRange = injectedResponse.request().headers()['range'] ?? '';

	return { response: injectedResponse, requestedRange };
}

test('demo audio plays and advances in the browser', async ({ page }) => {
	await page.goto('/demo/player');

	const audio = page.getByTestId('demo-audio');
	await expect(audio).toBeVisible();
	await page.getByTestId('queue-select-demo-track').click();

	let playbackState = '';
	await expect
		.poll(
			async () => {
				playbackState = await audio.evaluate((element) => {
					const player = element as HTMLAudioElement;
					if (player.error) return `error:${player.error.code}`;
					if (!player.paused && player.currentTime > 0) return 'playing';
					return `waiting: paused=${player.paused}, currentTime=${player.currentTime}, readyState=${player.readyState}`;
				});
				return playbackState;
			},
			{ timeout: 15_000 }
		)
		.toMatch(/^(playing|error:\d+)$/);
	expect(playbackState, 'the selected demo track should decode and advance').toBe('playing');

	const media = await audio.evaluate((element) => {
		const player = element as HTMLAudioElement;
		return {
			durationIsFinite: Number.isFinite(player.duration),
			durationIsPositive: player.duration > 0,
			hasDecodedFrame: player.readyState >= HTMLMediaElement.HAVE_CURRENT_DATA,
			hasError: player.error !== null
		};
	});
	expect(media).toEqual({
		durationIsFinite: true,
		durationIsPositive: true,
		hasDecodedFrame: true,
		hasError: false
	});
});

test('seek storm does not count a response that ignores an offset Range request', async ({ page }) => {
	const { response, requestedRange } = await runSeekStormWithInjectedResponse(page, 200);
	expect(response.status()).toBe(200);
	expect(requestedRange).toMatch(RANGE_REQUEST_PATTERN);
	expect(Number(requestedRange.match(RANGE_REQUEST_PATTERN)?.[1])).toBeGreaterThan(0);

	await expect(page.getByTestId('seek-stats')).toContainText(/completed 1\b/);
	await expect(page.getByTestId('player-status')).toHaveText('Status: ready');
});

test('seek storm does not count a mismatched Content-Range response', async ({ page }) => {
	const { response, requestedRange } = await runSeekStormWithInjectedResponse(page, 206);
	expect(response.status()).toBe(206);
	expect(requestedRange).toMatch(RANGE_REQUEST_PATTERN);
	expect(Number(requestedRange.match(RANGE_REQUEST_PATTERN)?.[1])).toBeGreaterThan(0);

	const requestedMatch = requestedRange.match(RANGE_REQUEST_PATTERN);
	expect(requestedMatch).not.toBeNull();
	if (!requestedMatch) throw new Error(`unexpected Range request: ${requestedRange}`);
	const requestedStart = Number(requestedMatch[1]);
	const requestedEnd = Number(requestedMatch[2]);
	const contentRange = response.headers()['content-range'];
	const contentRangeMatch = contentRange?.match(/^bytes (\d+)-(\d+)\/(\d+)$/);
	expect(contentRangeMatch).not.toBeNull();
	if (!contentRangeMatch) throw new Error(`unexpected Content-Range: ${contentRange ?? '(missing)'}`);
	const responseStart = Number(contentRangeMatch[1]);
	const responseEnd = Number(contentRangeMatch[2]);
	const responseTotal = Number(contentRangeMatch[3]);
	expect(responseStart).not.toBe(requestedStart);
	expect(responseEnd - responseStart).toBe(requestedEnd - requestedStart);
	expect(responseTotal).toBe(MEDIA_BYTES);

	await expect(page.getByTestId('seek-stats')).toContainText(/completed 1\b/);
	await expect(page.getByTestId('player-status')).toHaveText('Status: ready');
});

test('artwork remains understandable when its image and placeholder both fail', async ({ page }) => {
	await page.goto('/demo/player');

	const placeholderSrc = await page
		.getByTestId('null-artwork')
		.getByTestId('track-artwork')
		.getAttribute('src');
	expect(placeholderSrc).toBeTruthy();
	if (!placeholderSrc) throw new Error('the demo placeholder image is missing');
	const placeholderPath = new URL(placeholderSrc, page.url()).pathname;

	await page.route('**/api/demo/missing-artwork.jpg*', (route) =>
		route.fulfill({ status: 404, contentType: 'text/plain', body: 'not found' })
	);
	await page.route(`**${placeholderPath}*`, (route) =>
		route.fulfill({ status: 404, contentType: 'text/plain', body: 'not found' })
	);

	const sourceFailure = page.waitForResponse(
		(response) => new URL(response.url()).pathname.endsWith('/api/demo/missing-artwork.jpg')
	);
	const placeholderFailure = page.waitForResponse(
		(response) => new URL(response.url()).pathname.endsWith(placeholderPath)
	);
	await page.reload();

	expect((await sourceFailure).status()).toBe(404);
	expect((await placeholderFailure).status()).toBe(404);
	await expect(
		page.getByTestId('broken-artwork').getByText('Broken demo artwork', { exact: true })
	).toBeVisible();
});
