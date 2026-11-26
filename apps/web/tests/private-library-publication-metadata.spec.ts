import { expect } from '@playwright/test';
import { PRIVATE_MUSIC } from '../../../e2e/helpers/private-music-journey.js';
import { freshTrialPublication, privateStagingAudio, PRIVATE_TRIAL_UPLOAD } from '../../../e2e/helpers/private-staging-fixture.js';
import { mockInvitedLibraryPlayback } from './fixtures/invited-library-playback.js';
import { fixtureCookie, hasFixtureSession, serveFlac, test } from './helpers/private-music-fixture.js';

test('the invited Library keeps late metadata with each same-title publication through selection', async ({
	browser, page, origin
}) => {
	const { library, player } = PRIVATE_MUSIC;
	const tones = privateStagingAudio();
	const upload = tones.find(({ file }) => file === 'tone-a.flac')!;
	const companionAudio = tones.find(({ file }) => file === 'tone-b.flac')!;
	const title = 'Synthetic Twin Tone';
	const publication = {
		id: '00000000-0000-4000-8000-000000000091', title, duration_ms: 14_000,
		artist: 'Synthetic Reed', release: 'Synthetic Reed Album'
	};
	// The protected upload accepts tone-a. The distinct tone-b publication is already persisted in the fixture.
	const companion = {
		id: '00000000-0000-4000-8000-000000000092', title, duration_ms: 14_000,
		artist: 'Synthetic Alder', release: 'Synthetic Alder Album'
	};
	const titleOnly = (track: typeof publication) => ({ ...track, artist: null, release: null });
	const rows = page.getByTestId('library-list').getByTestId(library.item);
	const row = (id: string) => rows.and(page.locator(`[${library.id}="${id}"]`));
	const rowIds = () => rows.evaluateAll((items, attribute) => items.map((item) => item.getAttribute(attribute)), library.id);
	const refresh = async () => {
		const [listing] = await Promise.all([
			page.waitForResponse(origin + library.api),
			page.getByRole('button', { name: library.refresh, exact: true }).click()
		]);
		expect(listing.status()).toBe(200);
	};
	let published = false;
	let enriched = false;
	const fixture = await mockInvitedLibraryPlayback(page, origin, {
		libraryItems: () => !published ? [titleOnly(companion)] : enriched
			? [companion, publication] : [titleOnly(publication), titleOnly(companion)],
		playableTrackId: (id) => published && (id === publication.id || id === companion.id) ? id : undefined,
		audio: (route) => serveFlac(route, new URL(route.request().url()).pathname.startsWith(`${player.plays}/media/${publication.id}/`)
			? upload.body : companionAudio.body)
	});
	await fixture.signIn();
	await page.goto(origin + library.path);
	await expect.poll(rowIds).toEqual([companion.id]);

	const owner = await browser.newContext({ serviceWorkers: 'block' });
	let publishedId: string;
	try {
		await owner.route('**/*', (route) => new URL(route.request().url()).origin === origin ? route.continue() : route.abort());
		await owner.addCookies([{ name: fixtureCookie, value: 'synthetic-owner', url: origin, httpOnly: true }]);
		await owner.route(origin + PRIVATE_TRIAL_UPLOAD, async (route) => {
			const request = route.request();
			if (request.method() !== 'POST' || await request.headerValue('origin') !== origin ||
				!hasFixtureSession(await request.headerValue('cookie'), 'synthetic-owner') ||
				await request.headerValue('authorization')) return route.fulfill({ status: 404 });
			if (!request.postDataBuffer()?.equals(upload.body)) return route.fulfill({ status: 400 });
			published = true;
			return route.fulfill({ json: { trackId: publication.id, sha256: upload.sha256 } });
		});
		const ownerPage = await owner.newPage();
		await ownerPage.goto(`${origin}/api/build`);
		const receipt = await ownerPage.evaluate(async ({ path, bytes }) => {
			const response = await fetch(path, {
				method: 'POST', headers: { 'Content-Type': 'audio/flac' }, body: Uint8Array.from(bytes)
			});
			return { status: response.status, body: await response.text() };
		}, { path: PRIVATE_TRIAL_UPLOAD, bytes: [...upload.body] });
		expect(receipt.status).toBe(200);
		publishedId = freshTrialPublication(receipt.body, upload.sha256, [companion.id]).trackId;
	} finally {
		await owner.close();
	}

	const selection = (id: string) => row(id).getByRole('checkbox', { name: `Select ${title}`, exact: true });
	const queue = page.getByTestId(player.queueItem);
	const active = page.locator(player.currentQueueItem);
	const audio = page.getByTestId(player.audio);
	const expectLabels = async (queued = false) => {
		for (const { id, artist } of [publication, companion]) {
			await expect(row(id).getByRole('button', {
				name: `${title} — ${artist}${queued ? ' — Queued' : ''}`, exact: true
			})).toBeVisible();
		}
	};
	await refresh();
	await expect.poll(rowIds).toEqual([publishedId, companion.id]);
	for (const id of [publishedId, companion.id]) {
		await expect(row(id).getByRole('button', { name: title, exact: true })).toBeVisible();
	}
	await selection(publishedId).check();
	await expect(selection(companion.id)).not.toBeChecked();

	enriched = true;
	await refresh();
	await expect.poll(rowIds).toEqual([companion.id, publishedId]);
	await expectLabels();
	await expect(selection(publishedId)).toBeChecked();
	await expect(selection(companion.id)).not.toBeChecked();
	await selection(companion.id).check();
	await page.getByRole('button', { name: 'Append selected', exact: true }).click();
	await expect.poll(() => queue.evaluateAll((items, attribute) =>
		items.map((item) => item.getAttribute(attribute)), player.queueId)).toEqual([publishedId, companion.id]);
	await expect(active).toHaveAttribute(player.queueId, publishedId);
	await expect(audio).toHaveCount(0);

	// Album is exposed by the player. Check it together with the active ID and the actual grant.
	const expectPlayingMetadata = async (play: () => Promise<void>, track: typeof publication) => {
		const [grant] = await Promise.all([page.waitForResponse(origin + player.plays), play()]);
		expect(grant.status()).toBe(200);
		expect(grant.request().postDataJSON().track_id).toBe(track.id);
		await expect(active).toHaveAttribute(player.queueId, track.id);
		await expect(audio).toHaveAttribute('src', fixture.mediaPath(track.id));
		await expect.poll(() => audio.evaluate((element: HTMLAudioElement) => !element.paused && element.currentTime > 0)).toBe(true);
		await expect.poll(() => page.evaluate(() => {
			const metadata = navigator.mediaSession?.metadata;
			return metadata && { title: metadata.title, artist: metadata.artist, album: metadata.album };
		})).toEqual({ title, artist: track.artist, album: track.release });
	};
	await expectPlayingMetadata(() => page.getByRole('region', { name: 'Player', exact: true })
		.getByRole('button', { name: 'Play', exact: true }).click(), publication);
	await expectPlayingMetadata(() => page.getByTestId(player.next).click(), companion);
	await expect.poll(rowIds).toEqual([companion.id, publishedId]);
	await expectLabels(true);
});
