import { expect } from '@playwright/test';
import { PRIVATE_MUSIC } from '../../../e2e/helpers/private-music-journey.js';
import { freshTrialPublication, privateStagingAudio, PRIVATE_TRIAL_UPLOAD } from '../../../e2e/helpers/private-staging-fixture.js';
import { LIBRARY_AUTH_CONTEXT_KEY, LIBRARY_SNAPSHOT_KEY } from '../src/lib/library/cache.js';
import { mockInvitedLibraryPlayback } from './fixtures/invited-library-playback.js';
import { fixtureCookie, hasFixtureSession, test } from './helpers/private-music-fixture.js';

test('the invited Library reload preserves the published title and duration', async ({ browser, page, origin }) => {
	const upload = privateStagingAudio().find(({ file }) => file === 'tone-a.flac')!;
	const publishedRow = {
		id: '00000000-0000-4000-8000-000000000084',
		title: 'Synthetic Reload Tone',
		duration_ms: 14_000
	};
	let admitted = false;
	const listener = await mockInvitedLibraryPlayback(page, origin, {
		libraryItems: () => admitted ? [publishedRow] : [],
		playableTrackId: () => undefined,
		audio: upload.body
	});
	await listener.signIn();

	const owner = await browser.newContext({ serviceWorkers: 'block' });
	let receipt: { status: number; body: string };
	try {
		await owner.route('**/*', (route) => new URL(route.request().url()).origin === origin ? route.continue() : route.abort());
		await owner.addCookies([{ name: fixtureCookie, value: 'synthetic-owner', url: origin, httpOnly: true }]);
		await owner.route(`${origin}${PRIVATE_TRIAL_UPLOAD}`, async (route) => {
			const request = route.request();
			if (request.method() !== 'POST' || await request.headerValue('origin') !== origin ||
				!hasFixtureSession(await request.headerValue('cookie'), 'synthetic-owner') ||
				await request.headerValue('authorization')) return route.fulfill({ status: 404 });
			if (!request.postDataBuffer()?.equals(upload.body)) return route.fulfill({ status: 400 });
			admitted = true;
			return route.fulfill({ json: { trackId: publishedRow.id, sha256: upload.sha256 } });
		});
		const ownerPage = await owner.newPage();
		await ownerPage.goto(`${origin}/api/build`);
		receipt = await ownerPage.evaluate(async ({ path, bytes }) => {
			const response = await fetch(path, {
				method: 'POST', headers: { 'Content-Type': 'audio/flac' }, body: Uint8Array.from(bytes)
			});
			return { status: response.status, body: await response.text() };
		}, { path: PRIVATE_TRIAL_UPLOAD, bytes: [...upload.body] });
	} finally {
		await owner.close();
	}
	expect(receipt.status).toBe(200);
	const publication = freshTrialPublication(receipt.body, upload.sha256, []);

	const libraryUrl = `${origin}${PRIVATE_MUSIC.library.path}`;
	const row = page.getByTestId(PRIVATE_MUSIC.library.item).and(page.locator(`[data-id="${publication.trackId}"]`));
	const title = row.getByRole('button', { name: publishedRow.title, exact: true });
	const readSnapshot = () => page.evaluate(({ contextKey, snapshotKey }) => {
		const context = sessionStorage.getItem(contextKey);
		const snapshot = context && sessionStorage.getItem(`${snapshotKey}.${context}`);
		return snapshot ? JSON.parse(snapshot) : null;
	}, { contextKey: LIBRARY_AUTH_CONTEXT_KEY, snapshotKey: LIBRARY_SNAPSHOT_KEY });

	const firstLoadAt = new Date('2026-01-01T12:00:00Z');
	await page.clock.setFixedTime(firstLoadAt);
	await page.goto(libraryUrl);
	await expect(title).toBeVisible();
	await expect.poll(readSnapshot).toMatchObject({ savedAt: firstLoadAt.toISOString(), items: [publishedRow] });

	// A new snapshot timestamp proves the reload accepted a fresh listing with the same metadata.
	const reloadAt = new Date('2026-01-01T12:01:00Z');
	await page.clock.setFixedTime(reloadAt);
	const [listing, navigation] = await Promise.all([
		page.waitForResponse(`${origin}${PRIVATE_MUSIC.library.api}`),
		page.reload()
	]);
	expect(navigation?.ok()).toBe(true);
	expect(listing.status()).toBe(200);
	await expect(page).toHaveURL(libraryUrl);
	await expect(title).toBeVisible();
	await expect.poll(readSnapshot).toMatchObject({ savedAt: reloadAt.toISOString(), items: [publishedRow] });
});
