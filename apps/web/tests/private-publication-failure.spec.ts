import { expect } from '@playwright/test';
import { PRIVATE_MUSIC } from '../../../e2e/helpers/private-music-journey.js';
import { privateStagingAudio, PRIVATE_TRIAL_UPLOAD } from '../../../e2e/helpers/private-staging-fixture.js';
import { LIBRARY_AUTH_CONTEXT_KEY, LIBRARY_SNAPSHOT_KEY } from '../src/lib/library/cache.js';
import { mockInvitedLibraryPlayback } from './fixtures/invited-library-playback.js';
import { fixtureCookie, hasFixtureSession, test } from './helpers/private-music-fixture.js';

test('retrying a failed second publication preserves the first track selected for playback', async ({ browser, page, origin }) => {
	// The protected trial upload accepts tone-a only. Seed the earlier publication separately.
	const tones = privateStagingAudio();
	const upload = tones.find(({ file }) => file === 'tone-a.flac')!;
	const second = {
		id: '00000000-0000-4000-8000-000000000002',
		title: 'Retried Tone', duration_ms: 14_000
	};
	const published = {
		id: '00000000-0000-4000-8000-000000000001',
		title: 'Published Tone', duration_ms: 14_000
	};
	let retryCommitted = false;
	const listener = await mockInvitedLibraryPlayback(page, origin, {
		libraryItems: () => retryCommitted ? [second, published] : [published],
		playableTrackId: () => published.id,
		audio: tones.find(({ file }) => file === 'tone-b.flac')!.body
	});
	await listener.signIn();

	const { library, player } = PRIVATE_MUSIC;
	const rows = page.getByTestId(library.item);
	const publishedRow = rows.and(page.locator(`[${library.id}="${published.id}"]`));
	const active = page.locator(player.currentQueueItem);
	const audio = page.getByTestId(player.audio);
	const requestedTracks: string[] = [];
	page.on('request', (request) => {
		if (request.url() === `${origin}${player.plays}`) requestedTracks.push(request.postDataJSON().track_id);
	});
	const readSnapshot = () => page.evaluate(({ contextKey, snapshotKey }) => {
		const context = sessionStorage.getItem(contextKey);
		const snapshot = context && sessionStorage.getItem(`${snapshotKey}.${context}`);
		return snapshot ? JSON.parse(snapshot) : null;
	}, { contextKey: LIBRARY_AUTH_CONTEXT_KEY, snapshotKey: LIBRARY_SNAPSHOT_KEY });

	const before = new Date('2026-01-01T12:00:00Z');
	await page.clock.setFixedTime(before);
	await page.goto(`${origin}${library.path}`);
	await expect(publishedRow).toBeVisible();
	await expect.poll(readSnapshot).toEqual({ savedAt: before.toISOString(), items: [published] });
	await publishedRow.locator(library.select).click();
	await expect(active).toHaveAttribute(player.queueId, published.id);
	const refreshAt = async (time: string, items: (typeof published)[]) => {
		const date = new Date(time);
		await page.clock.setFixedTime(date);
		const [listing] = await Promise.all([
			page.waitForResponse(`${origin}${library.api}`),
			page.getByRole('button', { name: library.refresh, exact: true }).click()
		]);
		expect(listing.status()).toBe(200);
		await expect.poll(readSnapshot).toEqual({ savedAt: date.toISOString(), items });
	};

	const failure = { code: 'service_unavailable', message: 'service unavailable', retryable: true };
	let attempts = 0;
	const owner = await browser.newContext({ serviceWorkers: 'block' });
	try {
		await owner.route('**/*', (route) => new URL(route.request().url()).origin === origin ? route.continue() : route.abort());
		await owner.addCookies([{ name: fixtureCookie, value: 'synthetic-owner', url: origin, httpOnly: true }]);
		await owner.route(`${origin}${PRIVATE_TRIAL_UPLOAD}`, async (route) => {
			const call = route.request();
			if (call.method() !== 'POST' || await call.headerValue('origin') !== origin ||
				!hasFixtureSession(await call.headerValue('cookie'), 'synthetic-owner') ||
				await call.headerValue('authorization')) return route.fulfill({ status: 404 });
			if (await call.headerValue('content-type') !== 'audio/flac' ||
				!call.postDataBuffer()?.equals(upload.body)) return route.fulfill({ status: 400 });
			if (++attempts === 1) return route.fulfill({ status: 503, json: failure });
			if (attempts !== 2) return route.fulfill({ status: 409 });
			retryCommitted = true;
			return route.fulfill({ json: { trackId: second.id, sha256: upload.sha256 } });
		});
		const ownerPage = await owner.newPage();
		await ownerPage.goto(`${origin}/api/build`);
		const postUpload = () => ownerPage.evaluate(async ({ path, bytes }) => {
			const response = await fetch(path, {
				method: 'POST', headers: { 'Content-Type': 'audio/flac' }, body: Uint8Array.from(bytes)
			});
			return { status: response.status, body: await response.json() };
		}, { path: PRIVATE_TRIAL_UPLOAD, bytes: [...upload.body] });
		expect(await postUpload()).toEqual({ status: 503, body: failure });
		await refreshAt('2026-01-01T12:01:00Z', [published]);
		await expect(publishedRow).toBeVisible();
		await expect(rows.and(page.locator(`[${library.id}="${second.id}"]`))).toHaveCount(0);
		await expect(rows).toHaveCount(1);
		await expect(active).toHaveAttribute(player.queueId, published.id);
		expect(await postUpload()).toEqual({ status: 200, body: { trackId: second.id, sha256: upload.sha256 } });
		expect(attempts).toBe(2);
	} finally {
		await owner.close();
	}

	// Put the retried track first in the listing to distinguish identity from position.
	await refreshAt('2026-01-01T12:02:00Z', [second, published]);
	await expect(rows).toHaveCount(2);
	await expect(rows.first()).toHaveAttribute(library.id, second.id);
	await expect(publishedRow).toBeVisible();
	await expect(rows.and(page.locator(`[${library.id}="${second.id}"]`))).toBeVisible();
	await expect(active).toHaveAttribute(player.queueId, published.id);
	await expect(audio).toHaveCount(0);
	expect(requestedTracks).toEqual([]);
	await page.getByRole('region', { name: 'Player', exact: true }).getByRole('button', { name: 'Play', exact: true }).click();
	await expect.poll(() => requestedTracks).toEqual([published.id]);
	await expect(audio).toHaveAttribute('src', listener.mediaPath(published.id));
	await expect.poll(() => audio.evaluate((element: HTMLAudioElement) => !element.paused && element.currentTime > 0.2)).toBe(true);
	expect(requestedTracks).toEqual([published.id]);
});
