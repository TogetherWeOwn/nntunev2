import { expect } from '@playwright/test';
import { PRIVATE_MUSIC } from '../../../e2e/helpers/private-music-journey.js';
import { privateStagingAudio, PRIVATE_TRIAL_UPLOAD } from '../../../e2e/helpers/private-staging-fixture.js';
import { LIBRARY_AUTH_CONTEXT_KEY, LIBRARY_SNAPSHOT_KEY } from '../src/lib/library/cache.js';
import { mockInvitedLibraryPlayback } from './fixtures/invited-library-playback.js';
import { fixtureCookie, hasFixtureSession, test } from './helpers/private-music-fixture.js';

test('a failed second publication leaves only the published track in the invited Library', async ({ browser, page, origin }) => {
	// The protected trial upload accepts tone-a only. Seed the earlier publication separately.
	const tones = privateStagingAudio();
	const staged = {
		id: '00000000-0000-4000-8000-000000000002',
		audio: tones.find(({ file }) => file === 'tone-a.flac')!.body
	};
	const published = {
		id: '00000000-0000-4000-8000-000000000001',
		title: 'Published Tone', duration_ms: 14_000
	};
	const listener = await mockInvitedLibraryPlayback(page, origin, {
		libraryItems: () => [published],
		playableTrackId: () => published.id,
		audio: tones.find(({ file }) => file === 'tone-b.flac')!.body
	});
	await listener.signIn();

	const { library } = PRIVATE_MUSIC;
	const rows = page.getByTestId(library.item);
	const publishedRow = rows.and(page.locator(`[${library.id}="${published.id}"]`));
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

	const failure = { code: 'service_unavailable', message: 'service unavailable', retryable: true };
	const owner = await browser.newContext({ serviceWorkers: 'block' });
	try {
		await owner.route('**/*', (route) => new URL(route.request().url()).origin === origin ? route.continue() : route.abort());
		await owner.addCookies([{ name: fixtureCookie, value: 'synthetic-owner', url: origin, httpOnly: true }]);
		await owner.route(`${origin}${PRIVATE_TRIAL_UPLOAD}`, async (route) => {
			const call = route.request();
			if (call.method() !== 'POST' || await call.headerValue('origin') !== origin ||
				!hasFixtureSession(await call.headerValue('cookie'), 'synthetic-owner') ||
				await call.headerValue('authorization')) return route.fulfill({ status: 404 });
			if (!call.postDataBuffer()?.equals(staged.audio)) return route.fulfill({ status: 400 });
			return route.fulfill({ status: 503, json: failure });
		});
		const ownerPage = await owner.newPage();
		await ownerPage.goto(`${origin}/api/build`);
		const result = await ownerPage.evaluate(async ({ path, bytes }) => {
			const response = await fetch(path, {
				method: 'POST', headers: { 'Content-Type': 'audio/flac' }, body: Uint8Array.from(bytes)
			});
			return { status: response.status, body: await response.json() };
		}, { path: PRIVATE_TRIAL_UPLOAD, bytes: [...staged.audio] });
		expect(result).toEqual({ status: 503, body: failure });
	} finally {
		await owner.close();
	}

	// A newly saved snapshot proves the invited tab accepted the post-failure listing.
	const after = new Date('2026-01-01T12:01:00Z');
	await page.clock.setFixedTime(after);
	const [listing] = await Promise.all([
		page.waitForResponse(`${origin}${library.api}`),
		page.getByRole('button', { name: library.refresh, exact: true }).click()
	]);
	expect(listing.status()).toBe(200);
	await expect.poll(readSnapshot).toEqual({ savedAt: after.toISOString(), items: [published] });
	await expect(publishedRow).toBeVisible();
	await expect(rows.and(page.locator(`[${library.id}="${staged.id}"]`))).toHaveCount(0);
	await expect(rows).toHaveCount(1);
});
